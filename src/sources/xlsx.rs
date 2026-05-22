use std::{collections::HashMap, fs::File, io::Read, path::Path};

use anyhow::{Context as _, Result, anyhow, bail};
use calamine::{Data, ExcelDateTime, Range, Reader as _, Xlsx, open_workbook};
use quick_xml::{Reader as XmlReader, events::Event};
use zip::ZipArchive;

use crate::workbook::{
    CellBorders, CellCoord, CellData, CellDisplayFormat, CellRange, CellRawValue, CellStyle,
    DEFAULT_COLUMN_WIDTH, DEFAULT_ROW_HEIGHT, SheetData, SheetFreeze, SheetMerges, WorkbookData,
    calculate_missing_formula_values, display_float,
};

pub(crate) fn load_xlsx(path: &Path) -> Result<WorkbookData> {
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
                        let value = display_cell(cell, style.display_format.as_ref());
                        let formula = formula_at(&formulas, row_ix, col_ix);
                        let formula_value_was_uncached = value.is_empty()
                            && formula
                                .as_deref()
                                .is_some_and(|formula| !formula.is_empty());
                        CellData {
                            value,
                            formula,
                            raw_value: raw_value(cell),
                            style: style.visual_style.clone(),
                            display_format: style.display_format,
                            formula_value_was_uncached,
                        }
                    })
                    .collect()
            })
            .collect();

        sheets.push(
            SheetData::from_eager_with_freeze(
                Some(sheet_name.clone()),
                rows,
                sheet_metadata.column_widths,
                sheet_metadata.row_heights,
                sheet_metadata.default_column_width,
                sheet_metadata.default_row_height,
                sheet_metadata.freeze,
            )
            .with_merges(SheetMerges::from_ranges(sheet_metadata.merges)),
        );
    }

    calculate_missing_formula_values(&mut sheets);

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

pub(crate) fn display_cell(cell: &Data, format: Option<&CellDisplayFormat>) -> String {
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

#[derive(Debug, Clone, Default)]
pub(crate) struct XlsxCellStyle {
    pub(crate) display_format: Option<CellDisplayFormat>,
    pub(crate) visual_style: CellStyle,
}

#[derive(Debug, Clone, Default)]
struct XlsxStyle {
    display_format: Option<CellDisplayFormat>,
    visual_style: CellStyle,
}

#[derive(Debug, Default)]
pub(crate) struct XlsxMetadata {
    sheets: HashMap<String, SheetMetadata>,
}

impl XlsxMetadata {
    pub(crate) fn read(path: &Path) -> Result<Self> {
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

    pub(crate) fn sheet_metadata(&self, sheet_name: &str) -> SheetMetadata {
        self.sheets.get(sheet_name).cloned().unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn cell_style(&self, row: usize, col: usize) -> XlsxCellStyle {
        self.sheets
            .values()
            .next()
            .map(|sheet| sheet.cell_style(row, col))
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SheetMetadata {
    cell_styles: HashMap<(usize, usize), XlsxCellStyle>,
    pub(crate) column_widths: Vec<f32>,
    pub(crate) row_heights: Vec<f32>,
    pub(crate) default_column_width: f32,
    pub(crate) default_row_height: f32,
    pub(crate) freeze: SheetFreeze,
    pub(crate) merges: Vec<CellRange>,
}

impl Default for SheetMetadata {
    fn default() -> Self {
        Self {
            cell_styles: HashMap::new(),
            column_widths: Vec::new(),
            row_heights: Vec::new(),
            default_column_width: DEFAULT_COLUMN_WIDTH,
            default_row_height: DEFAULT_ROW_HEIGHT,
            freeze: SheetFreeze::default(),
            merges: Vec::new(),
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
    let mut borders = Vec::new();
    let mut styles = Vec::new();
    let mut pending_xf = None;
    let mut in_fonts = false;
    let mut in_font = false;
    let mut font = CellStyle::default();
    let mut in_fills = false;
    let mut in_fill = false;
    let mut in_solid_pattern_fill = false;
    let mut fill_color = None;
    let mut in_borders = false;
    let mut in_border = false;
    let mut current_border = CellBorders::default();
    let mut current_side: Option<BorderSide> = None;
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
            Event::Start(event) if event.local_name().as_ref() == b"borders" => {
                in_borders = true;
            }
            Event::End(event) if event.local_name().as_ref() == b"borders" => {
                in_borders = false;
            }
            Event::Empty(event) if in_borders && event.local_name().as_ref() == b"border" => {
                borders.push(CellBorders::default());
            }
            Event::Start(event) if in_borders && event.local_name().as_ref() == b"border" => {
                in_border = true;
                current_border = CellBorders::default();
            }
            Event::End(event) if in_border && event.local_name().as_ref() == b"border" => {
                in_border = false;
                current_side = None;
                borders.push(current_border);
            }
            Event::Start(event) | Event::Empty(event)
                if in_border && border_side(event.local_name().as_ref()).is_some() =>
            {
                let side = border_side(event.local_name().as_ref()).expect("border side");
                let has_border =
                    attr_string(&reader, &event, b"style")?.is_some_and(|style| style != "none");
                if has_border {
                    set_border_side(&mut current_border, side, Some(DEFAULT_BORDER_COLOR));
                    current_side = Some(side);
                } else {
                    current_side = None;
                }
            }
            Event::End(event)
                if in_border && border_side(event.local_name().as_ref()).is_some() =>
            {
                current_side = None;
            }
            Event::Start(event) | Event::Empty(event)
                if in_border
                    && event.local_name().as_ref() == b"color"
                    && current_side.is_some() =>
            {
                if let Some(rgb) = attr_rgb(&reader, &event)? {
                    let side = current_side.expect("current border side");
                    set_border_side(&mut current_border, side, Some(rgb));
                }
            }
            Event::Empty(event) if in_cell_xfs && event.local_name().as_ref() == b"xf" => {
                styles.push(xlsx_style_from_xf(
                    &reader,
                    &event,
                    &number_formats,
                    &fonts,
                    &fills,
                    &borders,
                )?);
            }
            Event::Start(event) if in_cell_xfs && event.local_name().as_ref() == b"xf" => {
                pending_xf = Some(xlsx_style_from_xf(
                    &reader,
                    &event,
                    &number_formats,
                    &fonts,
                    &fills,
                    &borders,
                )?);
            }
            Event::Start(event) | Event::Empty(event)
                if pending_xf.is_some() && event.local_name().as_ref() == b"alignment" =>
            {
                if let Some(style) = pending_xf.as_mut()
                    && attr_bool(&reader, &event, b"wrapText")?.unwrap_or(false)
                {
                    style.visual_style.wrap_text = true;
                }
            }
            Event::End(event) if pending_xf.is_some() && event.local_name().as_ref() == b"xf" => {
                styles.push(pending_xf.take().expect("pending xf should exist"));
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(styles)
}

/// Default border color (black) used when a side declares a style but no
/// explicit color, matching how spreadsheet apps render `style`-only borders.
const DEFAULT_BORDER_COLOR: u32 = 0x00_00_00;

#[derive(Debug, Clone, Copy)]
enum BorderSide {
    Top,
    Right,
    Bottom,
    Left,
}

fn border_side(local_name: &[u8]) -> Option<BorderSide> {
    match local_name {
        b"top" => Some(BorderSide::Top),
        b"right" => Some(BorderSide::Right),
        b"bottom" => Some(BorderSide::Bottom),
        b"left" => Some(BorderSide::Left),
        _ => None,
    }
}

fn set_border_side(borders: &mut CellBorders, side: BorderSide, color: Option<u32>) {
    match side {
        BorderSide::Top => borders.top = color,
        BorderSide::Right => borders.right = color,
        BorderSide::Bottom => borders.bottom = color,
        BorderSide::Left => borders.left = color,
    }
}

fn xlsx_style_from_xf(
    reader: &XmlReader<&[u8]>,
    event: &quick_xml::events::BytesStart<'_>,
    number_formats: &HashMap<usize, String>,
    fonts: &[CellStyle],
    fills: &[Option<u32>],
    borders: &[CellBorders],
) -> Result<XlsxStyle> {
    let display_format = attr_usize(reader, event, b"numFmtId")?
        .and_then(|id| number_formats.get(&id))
        .and_then(|format_code| CellDisplayFormat::from_format_code(format_code));
    let font_style = attr_usize(reader, event, b"fontId")?
        .and_then(|id| fonts.get(id))
        .cloned()
        .unwrap_or_default();
    let background_color = attr_usize(reader, event, b"fillId")?
        .and_then(|id| fills.get(id))
        .copied()
        .flatten();
    let cell_borders = attr_usize(reader, event, b"borderId")?
        .and_then(|id| borders.get(id))
        .copied()
        .unwrap_or_default();

    Ok(XlsxStyle {
        display_format,
        visual_style: CellStyle {
            background_color,
            borders: cell_borders,
            ..font_style
        },
    })
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
            Event::Start(event) | Event::Empty(event)
                if event.local_name().as_ref() == b"pane"
                    && attr_string(&reader, &event, b"state")?.as_deref() == Some("frozen") =>
            {
                metadata.freeze = SheetFreeze {
                    rows: attr_f32(&reader, &event, b"ySplit")?.map_or(0, frozen_split_to_count),
                    columns: attr_f32(&reader, &event, b"xSplit")?.map_or(0, frozen_split_to_count),
                };
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
            Event::Start(event) | Event::Empty(event)
                if event.local_name().as_ref() == b"mergeCell" =>
            {
                if let Some(reference) = attr_string(&reader, &event, b"ref")?
                    && let Some(range) = parse_merge_ref(&reference)
                {
                    metadata.merges.push(range);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    Ok(metadata)
}

/// Parse an `A1:B2`-style merged-range reference into a [`CellRange`].
fn parse_merge_ref(reference: &str) -> Option<CellRange> {
    let (start, end) = reference.split_once(':')?;
    let (start_row, start_col) = cell_ref_to_indices(start.trim())?;
    let (end_row, end_col) = cell_ref_to_indices(end.trim())?;
    Some(CellRange::new(
        CellCoord::new(start_row, start_col),
        CellCoord::new(end_row, end_col),
    ))
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn frozen_split_to_count(split: f32) -> usize {
    split.max(0.0).round() as usize
}

fn set_vec_value(values: &mut Vec<f32>, ix: usize, value: f32) {
    if values.len() <= ix {
        values.resize(ix + 1, 0.0);
    }
    values[ix] = value;
}

pub(crate) fn excel_column_width_to_px(width: f32) -> f32 {
    (width * 7.0 + 5.0).max(24.0)
}

pub(crate) fn points_to_px(points: f32) -> f32 {
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

fn attr_bool(
    reader: &XmlReader<&[u8]>,
    event: &quick_xml::events::BytesStart<'_>,
    name: &[u8],
) -> Result<Option<bool>> {
    Ok(attr_string(reader, event, name)?
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE")))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_merged_range_references() {
        let range = parse_merge_ref("A1:I1").expect("valid ref");
        assert_eq!(range.start, CellCoord::new(0, 0));
        assert_eq!(range.end, CellCoord::new(0, 8));

        let range = parse_merge_ref("B3:D6").expect("valid ref");
        assert_eq!(range.start, CellCoord::new(2, 1));
        assert_eq!(range.end, CellCoord::new(5, 3));

        assert!(parse_merge_ref("A1").is_none());
        assert!(parse_merge_ref("nonsense").is_none());
    }
}
