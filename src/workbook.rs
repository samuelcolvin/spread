use std::{
    any::Any,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, Result, bail};
use formualizer_workbook::{LiteralValue, Workbook as FormulaWorkbook};
use serde::Serialize;

#[derive(Debug, Clone)]
pub(crate) struct WorkbookData {
    path: PathBuf,
    sheets: Vec<SheetData>,
}

#[derive(Debug, Clone)]
pub(crate) struct SheetData {
    name: String,
    source: Arc<dyn SheetSource>,
    freeze: SheetFreeze,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SheetFreeze {
    pub(crate) rows: usize,
    pub(crate) columns: usize,
}

#[derive(Debug, Clone)]
struct EagerSheetSource {
    rows: Vec<Vec<CellData>>,
    column_widths: Vec<f32>,
    row_heights: Vec<f32>,
    default_column_width: f32,
    default_row_height: f32,
    row_count: usize,
    col_count: usize,
}

#[derive(Debug, Clone)]
pub(crate) enum SheetRowLayout {
    Uniform { row_count: usize, height: f32 },
    Explicit { heights: Vec<f32> },
}

pub(crate) trait SheetSource: Any + Send + Sync + std::fmt::Debug {
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn row_count(&self) -> usize;
    fn col_count(&self) -> usize;
    fn cell_data(&self, row: usize, col: usize) -> CellData;
    fn column_width(&self, col: usize) -> f32;
    fn row_height(&self, row: usize) -> f32;
    fn row_layout(&self) -> SheetRowLayout;
    fn is_fully_loaded(&self) -> bool;
    fn supports_full_range_operations(&self) -> bool;
}

impl WorkbookData {
    pub(crate) fn new(path: PathBuf, sheets: Vec<SheetData>) -> Self {
        Self { path, sheets }
    }

    pub(crate) fn placeholder() -> Self {
        Self::new(
            PathBuf::from("spreadsheet"),
            vec![SheetData::new(
                None,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                DEFAULT_COLUMN_WIDTH,
                DEFAULT_ROW_HEIGHT,
            )],
        )
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

    pub(crate) fn sheet_names(&self) -> impl Iterator<Item = &str> {
        self.sheets.iter().map(SheetData::name)
    }

    pub(crate) fn sheet_index(&self, sheet: &str) -> Option<usize> {
        self.sheets
            .iter()
            .position(|candidate| candidate.name() == sheet)
            .or_else(|| {
                sheet
                    .parse::<usize>()
                    .ok()
                    .and_then(|sheet_ix| sheet_ix.checked_sub(1))
                    .filter(|sheet_ix| *sheet_ix < self.sheet_count())
            })
    }

    #[cfg(test)]
    pub(crate) fn row_count(&self) -> usize {
        self.sheet(0).row_count()
    }

    #[cfg(test)]
    pub(crate) fn col_count(&self) -> usize {
        self.sheet(0).col_count()
    }

    #[cfg(test)]
    pub(crate) fn cell(&self, row: usize, col: usize) -> String {
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

    pub(crate) fn formula_audits(&self, sheet_ix: Option<usize>) -> Result<Vec<FormulaAudit>> {
        let sheet_indices = sheet_ix.map_or_else(
            || (0..self.sheet_count()).collect(),
            |sheet_ix| vec![sheet_ix],
        );
        let mut workbook = build_formula_workbook(&self.sheets, FormulaWorkbookMode::AllFormulas)?;
        workbook.prepare_graph_all()?;

        sheet_indices
            .into_iter()
            .map(|sheet_ix| self.formula_audit_for_sheet(sheet_ix, &mut workbook))
            .collect()
    }

    fn formula_audit_for_sheet(
        &self,
        sheet_ix: usize,
        workbook: &mut FormulaWorkbook,
    ) -> Result<FormulaAudit> {
        let sheet = self.sheet(sheet_ix);
        let mut audit = FormulaAudit {
            sheet: sheet.name.clone(),
            ..Default::default()
        };

        for row_ix in 0..sheet.row_count() {
            for col_ix in 0..sheet.col_count() {
                let cell = sheet.cell_data(row_ix, col_ix);
                let Some(formula) = cell
                    .formula
                    .as_deref()
                    .filter(|formula| !formula.is_empty())
                else {
                    continue;
                };

                if cell.formula_value_was_uncached {
                    audit.uncached_values += 1;
                    continue;
                }

                let calculated_value =
                    match workbook.evaluate_cell(&sheet.name, coord(row_ix)?, coord(col_ix)?) {
                        Ok(value) => {
                            formula_value_to_cell_value(value, cell.display_format.as_ref())
                                .map_or_else(
                                    || "<unsupported value>".to_owned(),
                                    |(display_value, _)| display_value,
                                )
                        }
                        Err(error) => format!("<error: {error}>"),
                    };

                if calculated_value == cell.value {
                    audit.cached_matches += 1;
                } else {
                    audit.inconsistencies.push(FormulaInconsistency {
                        cell: cell_label(row_ix, col_ix),
                        formula: formula.to_owned(),
                        cached_value: cell.value,
                        calculated_value,
                    });
                }
            }
        }

        Ok(audit)
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
        Self::new_with_row_mode(
            sheet_name,
            rows,
            column_widths,
            row_heights,
            default_column_width,
            default_row_height,
            EagerRowHeightMode::Auto,
        )
    }

    fn new_with_row_mode(
        sheet_name: Option<String>,
        rows: Vec<Vec<CellData>>,
        column_widths: Vec<f32>,
        row_heights: Vec<f32>,
        default_column_width: f32,
        default_row_height: f32,
        row_height_mode: EagerRowHeightMode,
    ) -> Self {
        Self::new_with_freeze(
            sheet_name,
            rows,
            column_widths,
            row_heights,
            default_column_width,
            default_row_height,
            row_height_mode,
            SheetFreeze::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_with_freeze(
        sheet_name: Option<String>,
        rows: Vec<Vec<CellData>>,
        column_widths: Vec<f32>,
        row_heights: Vec<f32>,
        default_column_width: f32,
        default_row_height: f32,
        row_height_mode: EagerRowHeightMode,
        freeze: SheetFreeze,
    ) -> Self {
        let name = sheet_name.unwrap_or_else(|| "Sheet1".to_owned());
        let source = EagerSheetSource::new(
            rows,
            column_widths,
            row_heights,
            default_column_width,
            default_row_height,
            row_height_mode,
        );

        Self {
            name,
            source: Arc::new(source),
            freeze,
        }
    }

    pub(crate) fn from_eager_with_freeze(
        sheet_name: Option<String>,
        rows: Vec<Vec<CellData>>,
        column_widths: Vec<f32>,
        row_heights: Vec<f32>,
        default_column_width: f32,
        default_row_height: f32,
        freeze: SheetFreeze,
    ) -> Self {
        Self::new_with_freeze(
            sheet_name,
            rows,
            column_widths,
            row_heights,
            default_column_width,
            default_row_height,
            EagerRowHeightMode::Auto,
            freeze,
        )
    }

    pub(crate) fn from_source_with_freeze(
        sheet_name: Option<String>,
        source: impl SheetSource + 'static,
        freeze: SheetFreeze,
    ) -> Self {
        Self {
            name: sheet_name.unwrap_or_else(|| "Sheet1".to_owned()),
            source: Arc::new(source),
            freeze,
        }
    }

    fn eager_source_mut(&mut self) -> Option<&mut EagerSheetSource> {
        Arc::get_mut(&mut self.source)?
            .as_any_mut()
            .downcast_mut::<EagerSheetSource>()
    }

    pub(crate) fn row_layout(&self) -> SheetRowLayout {
        self.source.row_layout()
    }

    pub(crate) fn freeze(&self) -> SheetFreeze {
        self.freeze
    }

    pub(crate) fn is_fully_loaded(&self) -> bool {
        self.source.is_fully_loaded()
    }

    pub(crate) fn supports_full_range_operations(&self) -> bool {
        self.source.supports_full_range_operations()
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn row_count(&self) -> usize {
        self.source.row_count()
    }

    pub(crate) fn col_count(&self) -> usize {
        self.source.col_count()
    }

    #[cfg(test)]
    pub(crate) fn cell(&self, row: usize, col: usize) -> String {
        self.cell_data(row, col).value
    }

    pub(crate) fn cell_data(&self, row: usize, col: usize) -> CellData {
        self.source.cell_data(row, col)
    }

    pub(crate) fn column_width(&self, col: usize) -> f32 {
        self.source.column_width(col)
    }

    pub(crate) fn row_height(&self, row: usize) -> f32 {
        self.source.row_height(row)
    }

    #[cfg(test)]
    pub(crate) fn sheet_height(&self) -> f32 {
        match self.row_layout() {
            SheetRowLayout::Uniform { row_count, height } => row_count as f32 * height,
            SheetRowLayout::Explicit { heights } => heights.into_iter().sum(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum EagerRowHeightMode {
    Auto,
}

impl EagerSheetSource {
    fn new(
        rows: Vec<Vec<CellData>>,
        column_widths: Vec<f32>,
        row_heights: Vec<f32>,
        default_column_width: f32,
        default_row_height: f32,
        row_height_mode: EagerRowHeightMode,
    ) -> Self {
        let row_count = rows.len();
        let col_count = rows.iter().map(Vec::len).max().unwrap_or(0);
        let row_heights = match row_height_mode {
            EagerRowHeightMode::Auto => display_row_heights(
                &rows,
                row_heights,
                &column_widths,
                default_column_width,
                default_row_height,
                row_count,
            ),
        };

        Self {
            rows,
            column_widths,
            row_heights,
            default_column_width,
            default_row_height,
            row_count,
            col_count,
        }
    }
}

impl SheetSource for EagerSheetSource {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn row_count(&self) -> usize {
        self.row_count
    }

    fn col_count(&self) -> usize {
        self.col_count
    }

    fn cell_data(&self, row: usize, col: usize) -> CellData {
        self.rows
            .get(row)
            .and_then(|columns| columns.get(col))
            .cloned()
            .unwrap_or_default()
    }

    fn column_width(&self, col: usize) -> f32 {
        self.column_widths
            .get(col)
            .copied()
            .filter(|width| *width > 0.0)
            .unwrap_or(self.default_column_width)
    }

    fn row_height(&self, row: usize) -> f32 {
        self.row_heights
            .get(row)
            .copied()
            .filter(|height| *height > 0.0)
            .unwrap_or(self.default_row_height)
    }

    fn row_layout(&self) -> SheetRowLayout {
        if self.row_heights.is_empty()
            || self
                .row_heights
                .iter()
                .all(|height| (*height - self.default_row_height).abs() < f32::EPSILON)
        {
            SheetRowLayout::Uniform {
                row_count: self.row_count,
                height: self.default_row_height,
            }
        } else {
            SheetRowLayout::Explicit {
                heights: (0..self.row_count)
                    .map(|row_ix| self.row_height(row_ix))
                    .collect(),
            }
        }
    }

    fn is_fully_loaded(&self) -> bool {
        true
    }

    fn supports_full_range_operations(&self) -> bool {
        true
    }
}

impl SheetData {
    fn has_missing_formula_values(&self) -> bool {
        (0..self.row_count()).any(|row_ix| {
            (0..self.col_count()).any(|col_ix| {
                let cell = self.cell_data(row_ix, col_ix);
                cell.value.is_empty()
                    && cell
                        .formula
                        .as_deref()
                        .is_some_and(|formula| !formula.is_empty())
            })
        })
    }

    pub(crate) fn inspect(&self) -> InspectedSheet {
        debug_assert!(self.is_fully_loaded() || self.supports_full_range_operations());
        let row_count = self.row_count();
        let col_count = self.col_count();
        let mut cells = Vec::with_capacity(row_count * col_count);

        for row_ix in 0..row_count {
            for col_ix in 0..col_count {
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
            rows: row_count,
            cols: col_count,
            cells,
        }
    }

    pub(crate) fn summary_for_range(&self, range: CellRange) -> SelectionSummary {
        debug_assert!(self.supports_full_range_operations());
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

    pub(crate) fn range_to_tsv(&self, range: CellRange) -> String {
        debug_assert!(self.supports_full_range_operations());
        let range = range.normalized();
        if range.start == range.end {
            return self.cell_data(range.start.row, range.start.col).value;
        }

        let mut output = String::new();

        for row_ix in range.start.row..=range.end.row {
            if row_ix > range.start.row {
                output.push('\n');
            }

            for col_ix in range.start.col..=range.end.col {
                if col_ix > range.start.col {
                    output.push('\t');
                }

                append_clipboard_cell(&mut output, &self.cell_data(row_ix, col_ix).value);
            }
        }

        output
    }

    pub(crate) fn range_to_html(&self, range: CellRange) -> String {
        debug_assert!(self.supports_full_range_operations());
        let range = range.normalized();
        let mut output = String::new();
        output.push_str(
            r#"<html><head><meta charset="utf-8"></head><body><table cellspacing="0" cellpadding="0" style="border-collapse:collapse;">"#,
        );

        for row_ix in range.start.row..=range.end.row {
            write!(
                output,
                r#"<tr style="height:{:.2}px;">"#,
                self.row_height(row_ix)
            )
            .expect("writing to String should not fail");

            for col_ix in range.start.col..=range.end.col {
                let cell = self.cell_data(row_ix, col_ix);
                write!(
                    output,
                    r#"<td style="{}">"#,
                    clipboard_html_cell_style(
                        &cell,
                        self.column_width(col_ix),
                        self.row_height(row_ix)
                    )
                )
                .expect("writing to String should not fail");
                append_html_text(&mut output, &cell.value);
                output.push_str("</td>");
            }

            output.push_str("</tr>");
        }

        output.push_str("</table></body></html>");
        output
    }

    pub(crate) fn to_pretty_xml(&self) -> String {
        debug_assert!(self.supports_full_range_operations());
        let mut output = String::new();
        output.push_str("<sheet name=\"");
        append_xml_attr(&mut output, &self.name);
        output.push_str("\">\n");

        for row_ix in 0..self.row_count() {
            let row_tag = format!("row_{}", row_ix + 1);
            output.push_str("  <");
            output.push_str(&row_tag);
            output.push_str(">\n");

            for col_ix in 0..self.col_count() {
                let cell = self.cell_data(row_ix, col_ix);
                let col_tag = column_name(col_ix).to_ascii_lowercase();

                output.push_str("    <");
                output.push_str(&col_tag);
                if let Some(formula) = cell
                    .formula
                    .as_deref()
                    .filter(|formula| !formula.is_empty())
                {
                    output.push_str(" formula=\"");
                    append_xml_attr(&mut output, formula.strip_prefix('=').unwrap_or(formula));
                    output.push('"');
                }
                output.push('>');
                append_xml_text(&mut output, &cell.value);
                output.push_str("</");
                output.push_str(&col_tag);
                output.push_str(">\n");
            }

            output.push_str("  </");
            output.push_str(&row_tag);
            output.push_str(">\n");
        }

        output.push_str("</sheet>");
        output
    }

    fn numeric_value(&self, row: usize, col: usize) -> Option<f64> {
        match self.cell_data(row, col).raw_value {
            CellRawValue::Number(value) => Some(value),
            _ => None,
        }
    }
}

fn append_clipboard_cell(output: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '\t' | '\n' | '\r' => output.push(' '),
            _ => output.push(ch),
        }
    }
}

fn clipboard_html_cell_style(cell: &CellData, width: f32, height: f32) -> String {
    let font_weight = if cell.style.bold { "bold" } else { "normal" };
    format!(
        concat!(
            "border:1px solid #d9d9d9;",
            "padding:2px 8px;",
            "min-width:{:.2}px;",
            "width:{:.2}px;",
            "height:{:.2}px;",
            "color:#{};",
            "background-color:#{};",
            "font-weight:{};",
            "font-family:Arial,sans-serif;",
            "font-size:13px;",
            "white-space:pre-wrap;"
        ),
        width,
        width,
        height,
        css_color(cell.style.text_color.unwrap_or(0x20_21_24)),
        css_color(cell.style.background_color.unwrap_or(0xff_ff_ff)),
        font_weight,
    )
}

fn append_html_text(output: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            _ => output.push(ch),
        }
    }
}

fn append_xml_text(output: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(ch),
        }
    }
}

fn append_xml_attr(output: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            _ => output.push(ch),
        }
    }
}

fn display_row_heights(
    rows: &[Vec<CellData>],
    mut row_heights: Vec<f32>,
    column_widths: &[f32],
    default_column_width: f32,
    default_row_height: f32,
    row_count: usize,
) -> Vec<f32> {
    row_heights.resize(row_count, 0.0);

    for (row_ix, row_height) in row_heights.iter_mut().enumerate() {
        if *row_height <= 0.0 {
            *row_height = rows.get(row_ix).map_or(default_row_height, |row| {
                auto_row_height(row, column_widths, default_column_width, default_row_height)
            });
        }
    }

    row_heights
}

fn auto_row_height(
    row: &[CellData],
    column_widths: &[f32],
    default_column_width: f32,
    default_row_height: f32,
) -> f32 {
    let line_count = row
        .iter()
        .enumerate()
        .map(|(col_ix, cell)| {
            let width = column_widths
                .get(col_ix)
                .copied()
                .filter(|width| *width > 0.0)
                .unwrap_or(default_column_width);
            estimated_cell_line_count(cell, width)
        })
        .max()
        .unwrap_or(1);

    if line_count <= 1 {
        return default_row_height;
    }

    let line_height = (default_row_height - 4.0).max(14.0);
    default_row_height + ((line_count - 1) as f32 * line_height)
}

fn estimated_cell_line_count(cell: &CellData, width: f32) -> usize {
    if cell.value.is_empty() {
        return 1;
    }

    let has_line_breaks = cell.value.contains('\n');
    if !cell.style.wrap_text && !has_line_breaks {
        return 1;
    }

    cell.value
        .split('\n')
        .map(|line| {
            if cell.style.wrap_text {
                wrapped_line_count(line, width)
            } else {
                1
            }
        })
        .sum::<usize>()
        .max(1)
}

fn wrapped_line_count(line: &str, width: f32) -> usize {
    let available_width = (width - AUTO_ROW_HORIZONTAL_PADDING).max(AUTO_ROW_CHAR_WIDTH);
    let mut line_count = 1;
    let mut current_width = 0.0;

    for _ in line.chars() {
        if current_width > 0.0 && current_width + AUTO_ROW_CHAR_WIDTH > available_width {
            line_count += 1;
            current_width = 0.0;
        }
        current_width += AUTO_ROW_CHAR_WIDTH;
    }

    line_count
}

pub(crate) const DEFAULT_COLUMN_WIDTH: f32 = 120.0;
pub(crate) const DEFAULT_ROW_HEIGHT: f32 = 24.0;
const AUTO_ROW_CHAR_WIDTH: f32 = 7.0;
const AUTO_ROW_HORIZONTAL_PADDING: f32 = 8.0;

#[derive(Debug, Clone, Default)]
pub(crate) struct CellData {
    pub(crate) value: String,
    pub(crate) formula: Option<String>,
    pub(crate) raw_value: CellRawValue,
    pub(crate) style: CellStyle,
    pub(crate) display_format: Option<CellDisplayFormat>,
    pub(crate) formula_value_was_uncached: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct FormulaAudit {
    pub(crate) sheet: String,
    pub(crate) uncached_values: usize,
    pub(crate) cached_matches: usize,
    pub(crate) inconsistencies: Vec<FormulaInconsistency>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FormulaInconsistency {
    pub(crate) cell: String,
    pub(crate) formula: String,
    pub(crate) cached_value: String,
    pub(crate) calculated_value: String,
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
    pub(crate) wrap_text: bool,
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
        Some("parquet") => load_parquet(path),
        Some("xlsx") => crate::sources::xlsx::load_xlsx(path),
        Some(extension) => {
            bail!("unsupported file extension '.{extension}'; expected .csv, .parquet, or .xlsx")
        }
        None => bail!("unsupported file without extension; expected .csv, .parquet, or .xlsx"),
    }
}

fn load_csv(path: &Path) -> Result<WorkbookData> {
    Ok(WorkbookData::new(
        path.to_owned(),
        vec![crate::sources::csv::load_csv_sheet(path)?],
    ))
}

fn load_parquet(path: &Path) -> Result<WorkbookData> {
    Ok(WorkbookData::new(
        path.to_owned(),
        vec![crate::sources::parquet::load_parquet_sheet(path)?],
    ))
}

pub(crate) fn calculate_missing_formula_values(sheets: &mut [SheetData]) {
    if !sheets.iter().any(SheetData::has_missing_formula_values) {
        return;
    }

    let Ok(mut workbook) = build_formula_workbook(sheets, FormulaWorkbookMode::MissingOnly) else {
        return;
    };
    if workbook.prepare_graph_all().is_err() {
        return;
    }

    for sheet in sheets {
        let sheet_name = sheet.name.clone();
        let Some(source) = sheet.eager_source_mut() else {
            continue;
        };

        for row_ix in 0..source.row_count {
            for col_ix in 0..source.col_count {
                let Some(cell) = source
                    .rows
                    .get_mut(row_ix)
                    .and_then(|columns| columns.get_mut(col_ix))
                else {
                    continue;
                };

                if cell.formula.as_deref().is_none_or(str::is_empty) || !cell.value.is_empty() {
                    continue;
                }

                let (Some(row), Some(col)) = (coord_u32(row_ix), coord_u32(col_ix)) else {
                    continue;
                };
                let Ok(value) = workbook.evaluate_cell(&sheet_name, row, col) else {
                    continue;
                };
                let Some((display_value, raw_value)) =
                    formula_value_to_cell_value(value, cell.display_format.as_ref())
                else {
                    continue;
                };

                cell.value = display_value;
                cell.raw_value = raw_value;
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum FormulaWorkbookMode {
    MissingOnly,
    AllFormulas,
}

fn build_formula_workbook(
    sheets: &[SheetData],
    mode: FormulaWorkbookMode,
) -> Result<FormulaWorkbook> {
    let mut workbook = FormulaWorkbook::new();

    for sheet in sheets {
        workbook.add_sheet(&sheet.name)?;
    }

    for sheet in sheets {
        for row_ix in 0..sheet.row_count() {
            for col_ix in 0..sheet.col_count() {
                let cell = sheet.cell_data(row_ix, col_ix);
                if let Some(formula) = cell
                    .formula
                    .as_deref()
                    .filter(|formula| !formula.is_empty())
                    .filter(|_| {
                        matches!(mode, FormulaWorkbookMode::AllFormulas) || cell.value.is_empty()
                    })
                {
                    let formula = formula_with_equals(formula);
                    workbook.set_formula(&sheet.name, coord(row_ix)?, coord(col_ix)?, &formula)?;
                } else if let Some(value) = cell_value_to_formula_value(&cell) {
                    workbook.set_value(&sheet.name, coord(row_ix)?, coord(col_ix)?, value)?;
                }
            }
        }
    }

    Ok(workbook)
}

fn coord(index: usize) -> Result<u32> {
    u32::try_from(index + 1).with_context(|| format!("cell coordinate {index} is too large"))
}

fn coord_u32(index: usize) -> Option<u32> {
    u32::try_from(index + 1).ok()
}

fn cell_value_to_formula_value(cell: &CellData) -> Option<LiteralValue> {
    match cell.raw_value {
        CellRawValue::Empty => {
            (!cell.value.is_empty()).then(|| LiteralValue::Text(cell.value.clone()))
        }
        CellRawValue::Number(value) => Some(LiteralValue::Number(value)),
        CellRawValue::Bool(value) => Some(LiteralValue::Boolean(value)),
        CellRawValue::Text | CellRawValue::DateTime => Some(LiteralValue::Text(cell.value.clone())),
    }
}

fn formula_value_to_cell_value(
    value: LiteralValue,
    format: Option<&CellDisplayFormat>,
) -> Option<(String, CellRawValue)> {
    match value {
        LiteralValue::Empty => Some((String::new(), CellRawValue::Empty)),
        LiteralValue::Int(value) => Some((
            format.map_or_else(
                || value.to_string(),
                |format| format.format_number(value as f64),
            ),
            CellRawValue::Number(value as f64),
        )),
        LiteralValue::Number(value) => Some((
            format.map_or_else(
                || display_float(value),
                |format| format.format_number(value),
            ),
            CellRawValue::Number(value),
        )),
        LiteralValue::Boolean(value) => Some((value.to_string(), CellRawValue::Bool(value))),
        LiteralValue::Text(value) => Some((value, CellRawValue::Text)),
        LiteralValue::Date(value) => Some((value.to_string(), CellRawValue::DateTime)),
        LiteralValue::DateTime(value) => Some((value.to_string(), CellRawValue::DateTime)),
        LiteralValue::Time(value) => Some((value.to_string(), CellRawValue::DateTime)),
        LiteralValue::Duration(value) => Some((value.to_string(), CellRawValue::DateTime)),
        _ => None,
    }
}

fn formula_with_equals(formula: &str) -> String {
    if formula.starts_with('=') {
        formula.to_owned()
    } else {
        format!("={formula}")
    }
}

pub(crate) fn display_float(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

#[derive(Debug, Clone)]
pub(crate) enum CellDisplayFormat {
    Currency { decimals: usize },
    Percentage { decimals: usize },
}

impl CellDisplayFormat {
    pub(crate) fn from_format_code(format_code: &str) -> Option<Self> {
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

    pub(crate) fn format_number(&self, value: f64) -> String {
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

fn color_hex(color: u32) -> String {
    format!("{color:06x}")
}

fn css_color(color: u32) -> String {
    format!("{color:06x}")
}

fn round2(value: f32) -> f32 {
    (value * 100.0).round() / 100.0
}

pub(crate) fn column_label(index: usize) -> String {
    column_name(index)
}

fn cell_label(row: usize, col: usize) -> String {
    format!("{}{}", column_name(col), row + 1)
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
    use arrow_array::{BooleanArray, Int64Array, RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};
    use calamine::Data;
    use parquet::{arrow::ArrowWriter, file::properties::WriterProperties};
    use std::{
        env,
        fs::{self, File},
        io::Write as _,
        sync::Arc,
        time::SystemTime,
    };
    use zip::{ZipWriter, write::SimpleFileOptions};

    use crate::sources::xlsx::{
        XlsxMetadata, display_cell, excel_column_width_to_px, load_xlsx, points_to_px,
    };

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
    fn csv_uses_uniform_row_layout() {
        let path = temp_file("spread-layout.csv");
        fs::write(&path, "name,note\nAda,\"line 1\nline 2\"\n").unwrap();

        let workbook = load_csv(&path).unwrap();

        assert!(!workbook.sheet(0).is_fully_loaded());
        assert_eq!(workbook.cell(1, 1), "line 1\nline 2");
        assert!(matches!(
            workbook.sheet(0).row_layout(),
            SheetRowLayout::Uniform { row_count: 2, height }
                if (height - DEFAULT_ROW_HEIGHT).abs() < f32::EPSILON
        ));

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn loads_parquet_lazily_with_schema_header() {
        let path = temp_file("spread-test.parquet");
        write_parquet_file(&path);

        let workbook = load_workbook(&path).unwrap();

        assert!(!workbook.sheet(0).is_fully_loaded());
        assert_eq!(
            workbook.sheet_name(0),
            path.file_stem().unwrap().to_str().unwrap()
        );
        assert_eq!(workbook.row_count(), 4);
        assert_eq!(workbook.col_count(), 3);
        assert_eq!(workbook.cell(0, 0), "name");
        assert_eq!(workbook.cell(0, 1), "score");
        assert_eq!(workbook.cell(1, 0), "Ada");
        assert_eq!(workbook.cell(3, 0), "Linus");
        assert_eq!(workbook.cell(3, 1), "30");
        assert_eq!(
            workbook.sheet(0).freeze(),
            SheetFreeze {
                rows: 1,
                columns: 0,
            }
        );
        assert_eq!(
            workbook.cell_data(3, 1).raw_value,
            CellRawValue::Number(30.0)
        );
        assert_eq!(
            workbook.cell_data(2, 2).raw_value,
            CellRawValue::Bool(false)
        );
        assert!(matches!(
            workbook.sheet(0).row_layout(),
            SheetRowLayout::Uniform { row_count: 4, height }
                if (height - DEFAULT_ROW_HEIGHT).abs() < f32::EPSILON
        ));

        let summary = workbook
            .sheet(0)
            .summary_for_range(CellRange::new(CellCoord::new(1, 1), CellCoord::new(3, 1)));
        assert_eq!(summary.numeric_cells, 3);
        assert!((summary.sum - 60.0).abs() < f64::EPSILON);

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn multiline_cells_expand_auto_row_height_without_overriding_explicit_height() {
        let sheet = SheetData::new(
            Some("Sheet1".to_owned()),
            vec![vec![CellData {
                value: "line 1\nline 2\nline 3".to_owned(),
                ..Default::default()
            }]],
            Vec::new(),
            Vec::new(),
            DEFAULT_COLUMN_WIDTH,
            DEFAULT_ROW_HEIGHT,
        );

        assert!(sheet.row_height(0) > DEFAULT_ROW_HEIGHT);
        assert!(matches!(
            sheet.row_layout(),
            SheetRowLayout::Explicit { heights } if heights[0] > DEFAULT_ROW_HEIGHT
        ));

        let sheet = SheetData::new(
            Some("Sheet1".to_owned()),
            vec![vec![CellData {
                value: "line 1\nline 2\nline 3".to_owned(),
                ..Default::default()
            }]],
            Vec::new(),
            vec![30.0],
            DEFAULT_COLUMN_WIDTH,
            DEFAULT_ROW_HEIGHT,
        );

        assert!((sheet.row_height(0) - 30.0).abs() < 0.01);
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
        let traction = workbook.sheet(workbook.sheet_index("Open source traction").unwrap());
        assert!(traction.row_height(24) > DEFAULT_ROW_HEIGHT * 4.0);
        assert!(workbook.sheet_height() > 0.0);
    }

    #[test]
    fn calculates_missing_formula_cache_values() {
        let mut sheets = vec![
            SheetData::new(
                Some("Inputs".to_owned()),
                vec![vec![
                    CellData {
                        value: "2".to_owned(),
                        raw_value: CellRawValue::Number(2.0),
                        ..Default::default()
                    },
                    CellData {
                        value: "3".to_owned(),
                        raw_value: CellRawValue::Number(3.0),
                        ..Default::default()
                    },
                ]],
                Vec::new(),
                Vec::new(),
                DEFAULT_COLUMN_WIDTH,
                DEFAULT_ROW_HEIGHT,
            ),
            SheetData::new(
                Some("Summary".to_owned()),
                vec![vec![
                    CellData {
                        formula: Some("'Inputs'!A1+'Inputs'!B1".to_owned()),
                        ..Default::default()
                    },
                    CellData {
                        formula: Some("A1*4".to_owned()),
                        display_format: Some(CellDisplayFormat::Currency { decimals: 0 }),
                        ..Default::default()
                    },
                ]],
                Vec::new(),
                Vec::new(),
                DEFAULT_COLUMN_WIDTH,
                DEFAULT_ROW_HEIGHT,
            ),
        ];

        calculate_missing_formula_values(&mut sheets);

        assert_eq!(sheets[1].cell(0, 0), "5");
        assert_eq!(sheets[1].cell(0, 1), "$20");
        assert_eq!(
            sheets[1].cell_data(0, 1).raw_value,
            CellRawValue::Number(20.0)
        );
    }

    #[test]
    fn calculates_business_plan_formula_cache_values() {
        let path = Path::new("business_plan.xlsx");
        if !path.exists() {
            return;
        }

        let workbook = load_xlsx(path).unwrap();
        let overview = workbook.sheet(0);

        assert_eq!(overview.cell(3, 1), "18");
        assert_eq!(overview.cell(4, 1), "$1,080,000");
        assert_eq!(overview.cell(10, 1), "$1,561,000");
    }

    #[test]
    fn audits_formula_cache_inconsistencies() {
        let workbook = WorkbookData::new(
            PathBuf::from("book.xlsx"),
            vec![
                SheetData::new(
                    Some("Inputs".to_owned()),
                    vec![vec![
                        CellData {
                            value: "2".to_owned(),
                            raw_value: CellRawValue::Number(2.0),
                            ..Default::default()
                        },
                        CellData {
                            value: "3".to_owned(),
                            raw_value: CellRawValue::Number(3.0),
                            ..Default::default()
                        },
                    ]],
                    Vec::new(),
                    Vec::new(),
                    DEFAULT_COLUMN_WIDTH,
                    DEFAULT_ROW_HEIGHT,
                ),
                SheetData::new(
                    Some("Summary".to_owned()),
                    vec![vec![
                        CellData {
                            value: "5".to_owned(),
                            formula: Some("'Inputs'!A1+'Inputs'!B1".to_owned()),
                            raw_value: CellRawValue::Number(5.0),
                            ..Default::default()
                        },
                        CellData {
                            value: "999".to_owned(),
                            formula: Some("A1*4".to_owned()),
                            raw_value: CellRawValue::Number(999.0),
                            ..Default::default()
                        },
                        CellData {
                            value: "5".to_owned(),
                            formula: Some("'Inputs'!A1+'Inputs'!B1".to_owned()),
                            raw_value: CellRawValue::Number(5.0),
                            formula_value_was_uncached: true,
                            ..Default::default()
                        },
                    ]],
                    Vec::new(),
                    Vec::new(),
                    DEFAULT_COLUMN_WIDTH,
                    DEFAULT_ROW_HEIGHT,
                ),
            ],
        );

        let audit = workbook.formula_audits(Some(1)).unwrap().remove(0);

        assert_eq!(audit.sheet, "Summary");
        assert_eq!(audit.uncached_values, 1);
        assert_eq!(audit.cached_matches, 1);
        assert_eq!(
            audit.inconsistencies,
            vec![FormulaInconsistency {
                cell: "B1".to_owned(),
                formula: "A1*4".to_owned(),
                cached_value: "999".to_owned(),
                calculated_value: "20".to_owned(),
            }]
        );
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
    <xf fontId="1" fillId="2" numFmtId="0"><alignment wrapText="1"/></xf>
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
        assert!(style.wrap_text);
        assert!((sheet_metadata.default_column_width - excel_column_width_to_px(9.0)).abs() < 0.01);
        assert!((sheet_metadata.column_widths[1] - excel_column_width_to_px(20.0)).abs() < 0.01);
        assert!((sheet_metadata.column_widths[2] - excel_column_width_to_px(20.0)).abs() < 0.01);
        assert!((sheet_metadata.default_row_height - points_to_px(18.0)).abs() < 0.01);
        assert!((sheet_metadata.row_heights[3] - points_to_px(30.0)).abs() < 0.01);

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn reads_xlsx_freeze_panes_from_metadata() {
        let path = temp_file("spread-frozen.xlsx");
        let styles_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <cellXfs count="1"><xf numFmtId="0"/></cellXfs>
</styleSheet>"#;
        let sheet_xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetViews>
    <sheetView workbookViewId="0">
      <pane xSplit="2" ySplit="3" topLeftCell="C4" activePane="bottomRight" state="frozen"/>
    </sheetView>
  </sheetViews>
  <sheetData><row r="1"><c r="A1"/></row></sheetData>
</worksheet>"#;
        write_metadata_xlsx(&path, styles_xml, sheet_xml);

        let metadata = XlsxMetadata::read(&path).unwrap();
        let sheet_metadata = metadata.sheet_metadata("Sheet1");

        assert_eq!(
            sheet_metadata.freeze,
            SheetFreeze {
                rows: 3,
                columns: 2,
            }
        );

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
    fn range_to_tsv_uses_display_values_and_grid_separators() {
        let sheet = SheetData::new(
            Some("Sheet1".to_owned()),
            vec![
                vec![
                    CellData {
                        value: "2026-01-01".to_owned(),
                        raw_value: CellRawValue::DateTime,
                        ..Default::default()
                    },
                    CellData {
                        value: "$1,234.50".to_owned(),
                        raw_value: CellRawValue::Number(1234.5),
                        ..Default::default()
                    },
                ],
                vec![
                    CellData {
                        value: "line\nbreak".to_owned(),
                        raw_value: CellRawValue::Text,
                        ..Default::default()
                    },
                    CellData {
                        value: "15.2%".to_owned(),
                        raw_value: CellRawValue::Number(0.152),
                        ..Default::default()
                    },
                ],
            ],
            Vec::new(),
            Vec::new(),
            DEFAULT_COLUMN_WIDTH,
            DEFAULT_ROW_HEIGHT,
        );

        let copied = sheet.range_to_tsv(CellRange::new(CellCoord::new(1, 1), CellCoord::new(0, 0)));

        assert_eq!(copied, "2026-01-01\t$1,234.50\nline break\t15.2%");
    }

    #[test]
    fn single_cell_tsv_preserves_multiline_text() {
        let sheet = SheetData::new(
            Some("Sheet1".to_owned()),
            vec![vec![CellData {
                value: "line\nbreak".to_owned(),
                raw_value: CellRawValue::Text,
                ..Default::default()
            }]],
            Vec::new(),
            Vec::new(),
            DEFAULT_COLUMN_WIDTH,
            DEFAULT_ROW_HEIGHT,
        );

        let copied = sheet.range_to_tsv(CellRange::single(CellCoord::new(0, 0)));

        assert_eq!(copied, "line\nbreak");
    }

    #[test]
    fn range_to_html_includes_display_values_and_cell_styles() {
        let sheet = SheetData::new(
            Some("Sheet1".to_owned()),
            vec![vec![CellData {
                value: "Ada & <Grace>".to_owned(),
                style: CellStyle {
                    bold: true,
                    background_color: Some(0xaa_bb_cc),
                    text_color: Some(0x11_22_33),
                    wrap_text: false,
                },
                ..Default::default()
            }]],
            vec![80.0],
            vec![28.0],
            DEFAULT_COLUMN_WIDTH,
            DEFAULT_ROW_HEIGHT,
        );

        let copied = sheet.range_to_html(CellRange::single(CellCoord::new(0, 0)));

        assert!(copied.contains("<table"));
        assert!(copied.contains("Ada &amp; &lt;Grace&gt;"));
        assert!(copied.contains("color:#112233;"));
        assert!(copied.contains("background-color:#aabbcc;"));
        assert!(copied.contains("font-weight:bold;"));
        assert!(copied.contains("width:80.00px;"));
        assert!(copied.contains("height:28.00px;"));
    }

    #[test]
    fn sheet_to_pretty_xml_escapes_values_and_formulas() {
        let sheet = SheetData::new(
            Some("Sheet & 1".to_owned()),
            vec![vec![
                CellData {
                    value: "Ada & <Grace>".to_owned(),
                    ..Default::default()
                },
                CellData {
                    value: "2".to_owned(),
                    formula: Some("='Enterprise Revenue'!T45 < 2".to_owned()),
                    raw_value: CellRawValue::Number(2.0),
                    ..Default::default()
                },
            ]],
            Vec::new(),
            Vec::new(),
            DEFAULT_COLUMN_WIDTH,
            DEFAULT_ROW_HEIGHT,
        );

        assert_eq!(
            sheet.to_pretty_xml(),
            concat!(
                "<sheet name=\"Sheet &amp; 1\">\n",
                "  <row_1>\n",
                "    <a>Ada &amp; &lt;Grace&gt;</a>\n",
                "    <b formula=\"'Enterprise Revenue'!T45 &lt; 2\">2</b>\n",
                "  </row_1>\n",
                "</sheet>"
            )
        );
    }

    #[test]
    fn workbook_finds_sheets_by_name_or_one_based_index() {
        let workbook = WorkbookData::new(
            PathBuf::from("book.xlsx"),
            vec![
                SheetData::new(
                    Some("Summary".to_owned()),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    DEFAULT_COLUMN_WIDTH,
                    DEFAULT_ROW_HEIGHT,
                ),
                SheetData::new(
                    Some("Details".to_owned()),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                    DEFAULT_COLUMN_WIDTH,
                    DEFAULT_ROW_HEIGHT,
                ),
            ],
        );

        assert_eq!(workbook.sheet_index("Details"), Some(1));
        assert_eq!(workbook.sheet_index("2"), Some(1));
        assert_eq!(workbook.sheet_index("0"), None);
        assert_eq!(workbook.sheet_index("Missing"), None);
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

    fn write_parquet_file(path: &Path) {
        let schema = Arc::new(Schema::new(vec![
            Field::new("name", DataType::Utf8, false),
            Field::new("score", DataType::Int64, false),
            Field::new("active", DataType::Boolean, false),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec!["Ada", "Grace", "Linus"])),
                Arc::new(Int64Array::from(vec![10, 20, 30])),
                Arc::new(BooleanArray::from(vec![true, false, true])),
            ],
        )
        .unwrap();
        let props = WriterProperties::builder()
            .set_max_row_group_size(2)
            .build();
        let file = File::create(path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, Some(props)).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();
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
