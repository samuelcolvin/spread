use std::{
    cell::Cell,
    ops::Range,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, App, Bounds, Context, CursorStyle, Div, Entity, FocusHandle, Focusable, FontWeight,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, Render, ScrollHandle, Stateful, TextRun, Window, actions, canvas, div, font, point,
    prelude::*, px, rgb,
};

use crate::{
    CloseFile,
    workbook::{
        CellCoord, CellData, CellRange, CellRawValue, SelectionEdgeSides, SheetData, SheetMerges,
        SheetRowLayout, WorkbookData,
    },
};

const MIN_ROW_HEADER_WIDTH: f32 = 48.0;
const ROW_HEADER_DIGIT_WIDTH: f32 = 7.0;
const ROW_HEADER_HORIZONTAL_PADDING: f32 = 14.0;
const HEADER_HEIGHT: f32 = 24.0;
const SCROLLBAR_SIZE: f32 = 12.0;
const MIN_THUMB_SIZE: f32 = 32.0;
const GRID_COLOR: u32 = 0xd9_d9_d9;
const HEADER_BG: u32 = 0xf8_f9_fa;
const HEADER_TEXT: u32 = 0x3c_40_43;
const SHEET_BG: u32 = 0xfa_fa_fa;
const CELL_BG: u32 = 0xff_ff_ff;
const CELL_TEXT: u32 = 0x20_21_24;
const TITLE_BAR_BG: u32 = 0xf3_f4_f7;
const FORMULA_FALLBACK_TEXT: u32 = 0x5f_63_68;
const SELECTED_CELL_BG: u32 = 0xe8_f0_fe;
const ACTIVE_TAB_BG: u32 = 0xff_ff_ff;
const HOVER_CELL_BG: u32 = 0xee_f2_f7;
const SELECTION_BORDER: u32 = 0x1a_73_e8;
/// Weight (out of 256) of the selection blue blended over a cell's real
/// background, so zebra striping and fills stay visible under the highlight
/// (matching Sheets) instead of being replaced by a flat color.
const SELECTION_TINT_WEIGHT: u32 = 33;
const SELECTION_INNER_BORDER: u32 = 0xa8_c7_fa;
const TITLE_BAR_HEIGHT: f32 = 40.0;
const FORMULA_BAR_HEIGHT: f32 = 36.0;
const FORMULA_BAR_LINE_HEIGHT: f32 = 17.0;
const FORMULA_BAR_VERTICAL_PADDING: f32 = 12.0;
const CELL_HORIZONTAL_PADDING: f32 = 16.0;
const FOOTER_HEIGHT: f32 = 32.0;
const RESIZE_HANDLE_SIZE: f32 = 6.0;
const FREEZE_HANDLE_HIT_SIZE: f32 = 8.0;
const FREEZE_HANDLE_VISUAL_SIZE: f32 = 4.0;
const FREEZE_LINE_SIZE: f32 = 6.0;
const FREEZE_LINE_COLOR: u32 = 0xb8_bd_c5;
const MIN_COLUMN_WIDTH: f32 = 24.0;
const MIN_ROW_HEIGHT: f32 = 18.0;
const VERTICAL_SCROLL_DRAG_UPDATE_INTERVAL: Duration = Duration::from_millis(50);
const LAZY_VERTICAL_SCROLL_DRAG_UPDATE_INTERVAL: Duration = Duration::from_millis(200);
const COLUMN_RENDER_OVERSCAN: usize = 2;
const ROW_RENDER_OVERSCAN: usize = 4;

pub(crate) const WINDOW_WIDTH: f32 = 1100.0;
pub(crate) const WINDOW_HEIGHT: f32 = 720.0;

/// Blend the selection blue over `base` so the cell's real background still
/// shows through, rather than being replaced by a flat highlight color.
fn selection_tint(base: u32) -> u32 {
    let channel = |shift: u32| {
        let b = (base >> shift) & 0xff;
        let o = (SELECTION_BORDER >> shift) & 0xff;
        (b * (256 - SELECTION_TINT_WEIGHT) + o * SELECTION_TINT_WEIGHT) / 256
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

actions!(spreadsheet_viewer, [CopySelection]);

pub(crate) struct SpreadsheetViewer {
    workbook: Arc<WorkbookData>,
    show_splash_after_close: Rc<Cell<bool>>,
    focus_handle: FocusHandle,
    name_box_focus: FocusHandle,
    /// `Some(buffer)` while the name box is being edited; `None` shows the
    /// current selection address instead.
    name_box_input: Option<String>,
    /// When `true` the whole name box buffer is "selected", so the next typed
    /// character replaces it (set by double-clicking the box).
    name_box_select_all: bool,
    /// Scrollable viewport `(width, height)` captured at the last render so
    /// keyboard-driven navigation can center a target without re-laying-out.
    scrollable_viewport: (f32, f32),
    active_sheet: usize,
    selection: Selection,
    selection_drag: Option<CellCoord>,
    summary_metric: SummaryMetric,
    show_summary_menu: bool,
    horizontal_scroll: ScrollHandle,
    tabs_scroll: ScrollHandle,
    /// Committed vertical scroll offset in scrollable-area pixels (0 = top).
    /// The scrollable body is virtualized manually; GPUI never sees all rows.
    vertical_offset: f32,
    scrollbar_drag: Option<ScrollbarDrag>,
    resize_drag: Option<ResizeDrag>,
    freeze_drag: Option<FreezeDrag>,
    layouts: Vec<SheetLayout>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Selection {
    anchor: CellCoord,
    range: CellRange,
}

impl Selection {
    fn single(coord: CellCoord) -> Self {
        Self {
            anchor: coord,
            range: CellRange::single(coord),
        }
    }

    fn extend_to(self, coord: CellCoord) -> Self {
        Self {
            anchor: self.anchor,
            range: CellRange::new(self.anchor, coord),
        }
    }

    fn select_range(anchor: CellCoord, end: CellCoord) -> Self {
        Self {
            anchor,
            range: CellRange::new(anchor, end),
        }
    }

    fn contains(self, row: usize, col: usize) -> bool {
        self.range.contains(row, col)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SummaryMetric {
    Sum,
    Mean,
    Min,
    Max,
}

impl SummaryMetric {
    const ALL: [Self; 4] = [Self::Sum, Self::Mean, Self::Min, Self::Max];

    fn label(self) -> &'static str {
        match self {
            Self::Sum => "Sum",
            Self::Mean => "Mean",
            Self::Min => "Min",
            Self::Max => "Max",
        }
    }
}

#[derive(Clone, Copy)]
struct CellRenderState {
    coord: CellCoord,
    selected: bool,
    selection_edges: SelectionEdgeSides,
    active: bool,
}

/// How a cell participates in a merged region.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MergeKind {
    /// Not part of any merge.
    None,
    /// Top-left cell of a merge; carries the content.
    Anchor,
    /// Non-anchor cell hidden underneath a merge.
    Covered,
}

#[derive(Clone)]
struct RowCell {
    data: CellData,
    text: String,
    text_width: f32,
    formula_fallback: bool,
    merge: MergeKind,
}

#[derive(Clone, Copy)]
enum ScrollbarDrag {
    Horizontal {
        pointer_offset: Pixels,
    },
    Vertical {
        pointer_offset: Pixels,
        scroll_position: Pixels,
        last_sheet_update: Instant,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScrollbarVisibility {
    horizontal: bool,
    vertical: bool,
}

#[derive(Clone)]
struct SheetLayout {
    column_widths: Vec<f32>,
    rows: RowLayout,
    sheet_width: f32,
    row_header_width: f32,
    pinned_rows: usize,
    pinned_columns: usize,
    merges: SheetMerges,
}

#[derive(Clone)]
enum RowLayout {
    Uniform {
        row_count: usize,
        height: f32,
    },
    Explicit {
        row_heights: Vec<f32>,
        row_offsets: Vec<f32>,
    },
}

impl SheetLayout {
    fn new(sheet: &SheetData) -> Self {
        let column_widths = (0..sheet.col_count())
            .map(|col_ix| sheet.column_width(col_ix))
            .collect::<Vec<_>>();
        let rows = RowLayout::new(sheet.row_layout());
        let sheet_width = column_widths.iter().sum();

        Self {
            column_widths,
            rows,
            sheet_width,
            row_header_width: row_header_width(sheet.row_count()),
            pinned_rows: sheet.freeze().rows.min(sheet.row_count()),
            pinned_columns: sheet.freeze().columns.min(sheet.col_count()),
            merges: sheet.merges().clone(),
        }
    }

    /// The merge region anchored at `(row, col)`, if `(row, col)` is an anchor.
    fn merge_anchor(&self, row: usize, col: usize) -> Option<CellRange> {
        self.merges.anchor_at(row, col)
    }

    /// The anchor coord covering `(row, col)`, if `(row, col)` is covered.
    fn merge_covered_anchor(&self, row: usize, col: usize) -> Option<CellCoord> {
        self.merges.covered_anchor(row, col)
    }

    /// How `(row, col)` participates in a merge.
    fn merge_kind(&self, row: usize, col: usize) -> MergeKind {
        if self.merge_anchor(row, col).is_some() {
            MergeKind::Anchor
        } else if self.merge_covered_anchor(row, col).is_some() {
            MergeKind::Covered
        } else {
            MergeKind::None
        }
    }

    /// Pixel `(width, height)` covered by a merge region.
    fn merged_size(&self, range: CellRange) -> (f32, f32) {
        let range = range.normalized();
        let width = (range.start.col..=range.end.col)
            .map(|col| self.column_width(col))
            .sum();
        let height = (range.start.row..=range.end.row)
            .map(|row| self.row_height(row))
            .sum();
        (width, height)
    }

    fn column_width(&self, col: usize) -> f32 {
        self.column_widths
            .get(col)
            .copied()
            .unwrap_or(MIN_COLUMN_WIDTH)
    }

    fn row_height(&self, row: usize) -> f32 {
        self.rows.row_height(row)
    }

    fn sheet_height(&self) -> f32 {
        self.rows.sheet_height()
    }

    fn pinned_column_width(&self) -> f32 {
        self.columns_width(0, self.pinned_columns)
    }

    fn pinned_row_height(&self) -> f32 {
        self.rows_height(0, self.pinned_rows)
    }

    fn scrollable_width(&self) -> f32 {
        (self.sheet_width - self.pinned_column_width()).max(0.0)
    }

    fn scrollable_height(&self) -> f32 {
        (self.sheet_height() - self.pinned_row_height()).max(0.0)
    }

    fn columns_width(&self, start_col: usize, end_col: usize) -> f32 {
        let end_col = end_col.min(self.column_widths.len());
        (start_col.min(end_col)..end_col)
            .map(|col_ix| self.column_width(col_ix))
            .sum()
    }

    fn visible_scrollable_column_range(
        &self,
        scroll_position: f32,
        viewport_width: f32,
    ) -> Range<usize> {
        let first_scrollable_col = self.pinned_columns.min(self.column_widths.len());
        let col_count = self.column_widths.len();
        if viewport_width <= 0.0 || first_scrollable_col >= col_count {
            return first_scrollable_col..first_scrollable_col;
        }

        let scroll_position = scroll_position.max(0.0);
        let viewport_end = scroll_position + viewport_width;
        let mut col_ix = first_scrollable_col;
        let mut offset = 0.0;

        while col_ix < col_count {
            let next_offset = offset + self.column_width(col_ix);
            if next_offset > scroll_position {
                break;
            }
            offset = next_offset;
            col_ix += 1;
        }

        let start_col = col_ix
            .saturating_sub(COLUMN_RENDER_OVERSCAN)
            .max(first_scrollable_col);
        let mut end_col = col_ix;
        while end_col < col_count && offset < viewport_end {
            offset += self.column_width(end_col);
            end_col += 1;
        }

        end_col = (end_col + COLUMN_RENDER_OVERSCAN).min(col_count);
        start_col..end_col
    }

    fn rows_height(&self, start_row: usize, end_row: usize) -> f32 {
        let end_row = end_row.min(self.rows.row_count());
        (start_row.min(end_row)..end_row)
            .map(|row_ix| self.row_height(row_ix))
            .sum()
    }

    /// Visible scrollable row range for the given scroll position and viewport,
    /// with overscan. Uses `RowLayout`'s offset math so this stays O(1) for
    /// uniform sheets (millions of rows) and O(log n) for explicit heights;
    /// never scans row-by-row.
    fn visible_scrollable_row_range(
        &self,
        scroll_position: f32,
        viewport_height: f32,
    ) -> Range<usize> {
        let first_scrollable_row = self.pinned_rows.min(self.rows.row_count());
        let row_count = self.rows.row_count();
        if viewport_height <= 0.0 || first_scrollable_row >= row_count {
            return first_scrollable_row..first_scrollable_row;
        }

        let top = scroll_position.max(0.0) + self.pinned_row_height();
        let (start_row, _) = self.row_offset_for_scroll_position(top);
        let (end_row, _) = self.row_offset_for_scroll_position(top + viewport_height);

        let start = start_row
            .saturating_sub(ROW_RENDER_OVERSCAN)
            .max(first_scrollable_row);
        let end = (end_row + 1 + ROW_RENDER_OVERSCAN).min(row_count);
        start..end
    }

    /// Distance from the top of the scrollable region to the top of `row_ix`.
    fn scrollable_row_top(&self, row_ix: usize) -> f32 {
        (self.scroll_position_for_row_offset(row_ix, 0.0) - self.pinned_row_height()).max(0.0)
    }

    fn set_pinned_columns(&mut self, columns: usize) -> bool {
        let columns = columns.min(self.column_widths.len());
        if self.pinned_columns == columns {
            return false;
        }
        self.pinned_columns = columns;
        true
    }

    fn set_pinned_rows(&mut self, rows: usize) -> bool {
        let rows = rows.min(self.rows.row_count());
        if self.pinned_rows == rows {
            return false;
        }
        self.pinned_rows = rows;
        true
    }

    fn scroll_position_for_row_offset(&self, row_ix: usize, offset_in_row: f32) -> f32 {
        self.rows
            .scroll_position_for_row_offset(row_ix, offset_in_row)
    }

    fn row_offset_for_scroll_position(&self, position: f32) -> (usize, f32) {
        self.rows.row_offset_for_scroll_position(position)
    }

    fn set_column_widths(&mut self, updates: &[(usize, f32)]) {
        for (col_ix, width) in updates {
            if let Some(column_width) = self.column_widths.get_mut(*col_ix) {
                *column_width = width.max(MIN_COLUMN_WIDTH);
            }
        }
        self.sheet_width = self.column_widths.iter().sum();
    }

    fn set_row_heights(&mut self, updates: &[(usize, f32)]) {
        self.rows.set_row_heights(updates);
    }
}

impl RowLayout {
    fn new(layout: SheetRowLayout) -> Self {
        match layout {
            SheetRowLayout::Uniform { row_count, height } => Self::Uniform {
                row_count,
                height: height.max(MIN_ROW_HEIGHT),
            },
            SheetRowLayout::Explicit { heights } => Self::from_explicit_heights(heights),
        }
    }

    fn from_explicit_heights(row_heights: Vec<f32>) -> Self {
        let row_heights = row_heights
            .into_iter()
            .map(|height| height.max(MIN_ROW_HEIGHT))
            .collect::<Vec<_>>();
        let row_offsets = row_offsets_for_heights(&row_heights);
        Self::Explicit {
            row_heights,
            row_offsets,
        }
    }

    fn row_height(&self, row: usize) -> f32 {
        match self {
            Self::Uniform { row_count, height } => {
                if row < *row_count {
                    *height
                } else {
                    MIN_ROW_HEIGHT
                }
            }
            Self::Explicit { row_heights, .. } => {
                row_heights.get(row).copied().unwrap_or(MIN_ROW_HEIGHT)
            }
        }
    }

    fn sheet_height(&self) -> f32 {
        match self {
            Self::Uniform { row_count, height } => *row_count as f32 * *height,
            Self::Explicit { row_offsets, .. } => row_offsets.last().copied().unwrap_or(0.0),
        }
    }

    fn row_count(&self) -> usize {
        match self {
            Self::Uniform { row_count, .. } => *row_count,
            Self::Explicit { row_heights, .. } => row_heights.len(),
        }
    }

    fn scroll_position_for_row_offset(&self, row_ix: usize, offset_in_row: f32) -> f32 {
        match self {
            Self::Uniform { row_count, height } => {
                if *row_count == 0 {
                    0.0
                } else {
                    row_ix.min(row_count - 1) as f32 * *height + offset_in_row
                }
            }
            Self::Explicit { row_offsets, .. } => {
                row_offsets.get(row_ix).copied().unwrap_or(0.0) + offset_in_row
            }
        }
    }

    fn row_offset_for_scroll_position(&self, position: f32) -> (usize, f32) {
        match self {
            Self::Uniform { row_count, height } => {
                if *row_count == 0 || *height <= 0.0 {
                    return (0, 0.0);
                }

                let row_ix = uniform_row_index_for_position(position, *height);
                let row_ix = row_ix.min(row_count - 1);
                let row_start = row_ix as f32 * *height;
                (row_ix, (position - row_start).max(0.0))
            }
            Self::Explicit {
                row_heights,
                row_offsets,
            } => {
                if row_heights.is_empty() {
                    return (0, 0.0);
                }

                let row_ix = row_offsets.partition_point(|offset| *offset <= position);
                let row_ix = row_ix.saturating_sub(1).min(row_heights.len() - 1);
                let row_start = row_offsets.get(row_ix).copied().unwrap_or(0.0);
                (row_ix, (position - row_start).max(0.0))
            }
        }
    }

    fn set_row_heights(&mut self, updates: &[(usize, f32)]) {
        match self {
            Self::Uniform { row_count, height } => {
                if updates.is_empty() {
                    return;
                }

                let first_height = updates[0].1.max(MIN_ROW_HEIGHT);
                if updates.len() == *row_count
                    && updates.iter().all(|(_, update_height)| {
                        (update_height.max(MIN_ROW_HEIGHT) - first_height).abs() < f32::EPSILON
                    })
                {
                    *height = first_height;
                    return;
                }

                let mut row_heights = vec![*height; *row_count];
                for (row_ix, update_height) in updates {
                    if let Some(row_height) = row_heights.get_mut(*row_ix) {
                        *row_height = update_height.max(MIN_ROW_HEIGHT);
                    }
                }
                *self = Self::from_explicit_heights(row_heights);
            }
            Self::Explicit {
                row_heights,
                row_offsets,
            } => {
                for (row_ix, height) in updates {
                    if let Some(row_height) = row_heights.get_mut(*row_ix) {
                        *row_height = height.max(MIN_ROW_HEIGHT);
                    }
                }
                *row_offsets = row_offsets_for_heights(row_heights);
            }
        }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn uniform_row_index_for_position(position: f32, row_height: f32) -> usize {
    (position.max(0.0) / row_height).floor() as usize
}

fn row_offsets_for_heights(row_heights: &[f32]) -> Vec<f32> {
    let mut offsets = Vec::with_capacity(row_heights.len() + 1);
    let mut current = 0.0;
    offsets.push(current);

    for height in row_heights {
        current += *height;
        offsets.push(current);
    }

    offsets
}

fn row_header_width(row_count: usize) -> f32 {
    let label_len = row_number_label(row_count.max(1)).len();
    (label_len as f32 * ROW_HEADER_DIGIT_WIDTH + ROW_HEADER_HORIZONTAL_PADDING)
        .max(MIN_ROW_HEADER_WIDTH)
}

fn row_number_label(row_number: usize) -> String {
    let raw = row_number.to_string();
    let mut output = String::with_capacity(raw.len() + raw.len() / 3);

    for (ix, ch) in raw.chars().enumerate() {
        if ix > 0 && (raw.len() - ix).is_multiple_of(3) {
            output.push(',');
        }
        output.push(ch);
    }

    output
}

fn freeze_target_column(layout: &SheetLayout, pointer_x: f32) -> usize {
    freeze_target_for_sizes(
        pointer_x,
        layout
            .column_widths
            .iter()
            .enumerate()
            .map(|(col_ix, _)| layout.column_width(col_ix)),
    )
}

fn freeze_target_row(layout: &SheetLayout, pointer_y: f32) -> usize {
    freeze_target_for_sizes(
        pointer_y,
        (0..layout.rows.row_count()).map(|row_ix| layout.row_height(row_ix)),
    )
}

fn freeze_target_for_sizes(position: f32, sizes: impl IntoIterator<Item = f32>) -> usize {
    if position <= 0.0 {
        return 0;
    }

    let mut boundary = 0.0;
    let mut count = 0;
    for size in sizes {
        let size = size.max(0.0);
        if position < boundary + size / 2.0 {
            return count;
        }
        boundary += size;
        count += 1;
    }
    count
}

fn vertical_scroll_drag_update_interval(is_fully_loaded: bool) -> Duration {
    if is_fully_loaded {
        VERTICAL_SCROLL_DRAG_UPDATE_INTERVAL
    } else {
        LAZY_VERTICAL_SCROLL_DRAG_UPDATE_INTERVAL
    }
}

#[derive(Clone, Debug)]
enum ResizeDrag {
    Columns {
        start_x: Pixels,
        columns: Vec<usize>,
        start_widths: Vec<f32>,
    },
    Rows {
        start_y: Pixels,
        rows: Vec<usize>,
        start_heights: Vec<f32>,
    },
}

#[derive(Clone, Copy, Debug)]
enum FreezeDrag {
    Columns { axis_origin_x: Pixels },
    Rows { axis_origin_y: Pixels },
}

impl SpreadsheetViewer {
    pub(crate) fn new(
        workbook: Arc<WorkbookData>,
        active_sheet: usize,
        show_splash_after_close: Rc<Cell<bool>>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        let name_box_focus = cx.focus_handle();

        let layouts: Vec<SheetLayout> = (0..workbook.sheet_count())
            .map(|sheet_ix| SheetLayout::new(workbook.sheet(sheet_ix)))
            .collect();

        Self {
            workbook,
            show_splash_after_close,
            focus_handle,
            name_box_focus,
            name_box_input: None,
            name_box_select_all: false,
            scrollable_viewport: (0.0, 0.0),
            active_sheet,
            selection: Selection::single(CellCoord::new(0, 0)),
            selection_drag: None,
            summary_metric: SummaryMetric::Sum,
            show_summary_menu: false,
            horizontal_scroll: ScrollHandle::new(),
            tabs_scroll: ScrollHandle::new(),
            vertical_offset: 0.0,
            scrollbar_drag: None,
            resize_drag: None,
            freeze_drag: None,
            layouts,
        }
    }

    fn active_sheet(&self) -> &SheetData {
        self.workbook.sheet(self.active_sheet)
    }

    fn active_layout(&self) -> &SheetLayout {
        &self.layouts[self.active_sheet]
    }

    fn active_layout_mut(&mut self) -> &mut SheetLayout {
        &mut self.layouts[self.active_sheet]
    }

    fn copy_selection(&mut self, _: &CopySelection, _: &mut Window, cx: &mut Context<'_, Self>) {
        let text = self.active_sheet().range_to_tsv(self.selection.range);
        let html = self.active_sheet().range_to_html(self.selection.range);
        write_rich_clipboard(&text, &html, cx);
    }

    fn close_file(&mut self, _: &CloseFile, window: &mut Window, cx: &mut Context<'_, Self>) {
        // Handle Close File inside the document window, not as a global app action.
        // GPUI menu actions are dispatched through the active window first; removing
        // this exact window here avoids guessing which app window should be closed.
        self.show_splash_after_close.set(true);
        let workbook = std::mem::replace(&mut self.workbook, Arc::new(WorkbookData::placeholder()));
        cx.background_executor()
            .spawn(async move {
                drop(workbook);
            })
            .detach();
        window.remove_window();
    }

    fn switch_sheet(&mut self, sheet_ix: usize) -> bool {
        if sheet_ix >= self.workbook.sheet_count() || sheet_ix == self.active_sheet {
            return false;
        }

        self.active_sheet = sheet_ix;
        self.selection = Selection::single(CellCoord::new(0, 0));
        self.name_box_input = None;
        self.name_box_select_all = false;
        self.selection_drag = None;
        self.show_summary_menu = false;
        self.horizontal_scroll.set_offset(point(px(0.0), px(0.0)));
        self.vertical_offset = 0.0;
        true
    }

    /// Resolve a covered merge cell to its anchor; other coords pass through.
    fn merge_anchor_coord(&self, coord: CellCoord) -> CellCoord {
        self.active_layout()
            .merge_covered_anchor(coord.row, coord.col)
            .unwrap_or(coord)
    }

    fn select_cell(&mut self, coord: CellCoord, extend: bool) -> bool {
        let coord = self.merge_anchor_coord(coord);
        let next_selection = if extend {
            self.selection.extend_to(coord)
        } else {
            Selection::single(coord)
        };
        let changed = self.selection != next_selection || self.show_summary_menu;
        self.selection = next_selection;
        self.selection_drag = Some(self.selection.anchor);
        self.resize_drag = None;
        self.show_summary_menu = false;
        changed
    }

    fn drag_to_cell(&mut self, coord: CellCoord) -> bool {
        let coord = self.merge_anchor_coord(coord);
        if let Some(anchor) = self.selection_drag {
            let next_selection = Selection::select_range(anchor, coord);
            if self.selection == next_selection && !self.show_summary_menu {
                return false;
            }
            self.selection = next_selection;
            self.show_summary_menu = false;
            return true;
        }

        false
    }

    fn select_col(&mut self, col: usize, extend: bool) -> bool {
        let max_row = self.active_sheet().row_count().saturating_sub(1);
        let anchor_col = if extend {
            self.selection.anchor.col
        } else {
            col
        };
        let next_selection =
            Selection::select_range(CellCoord::new(0, anchor_col), CellCoord::new(max_row, col));
        let changed = self.selection != next_selection || self.show_summary_menu;
        self.selection = next_selection;
        self.selection_drag = None;
        self.resize_drag = None;
        self.show_summary_menu = false;
        changed
    }

    fn select_row(&mut self, row: usize, extend: bool) -> bool {
        let max_col = self.active_sheet().col_count().saturating_sub(1);
        let anchor_row = if extend {
            self.selection.anchor.row
        } else {
            row
        };
        let next_selection =
            Selection::select_range(CellCoord::new(anchor_row, 0), CellCoord::new(row, max_col));
        let changed = self.selection != next_selection || self.show_summary_menu;
        self.selection = next_selection;
        self.selection_drag = None;
        self.resize_drag = None;
        self.show_summary_menu = false;
        changed
    }

    fn select_sheet(&mut self) -> bool {
        let sheet = self.active_sheet();
        let max_row = sheet.row_count().saturating_sub(1);
        let max_col = sheet.col_count().saturating_sub(1);
        let next_selection =
            Selection::select_range(CellCoord::new(0, 0), CellCoord::new(max_row, max_col));
        let changed = self.selection != next_selection || self.show_summary_menu;
        self.selection = next_selection;
        self.selection_drag = None;
        self.resize_drag = None;
        self.show_summary_menu = false;
        changed
    }

    fn apply_name_ref(&mut self, name_ref: NameRef) {
        let center_on = match name_ref {
            NameRef::Cell(coord) => {
                self.select_cell(coord, false);
                coord
            }
            NameRef::Column(col) => {
                self.select_col(col, false);
                CellCoord::new(0, col)
            }
            NameRef::Row(row) => {
                self.select_row(row, false);
                CellCoord::new(row, 0)
            }
        };
        self.scroll_to_cell_centered(center_on);
    }

    fn scroll_to_cell_centered(&mut self, coord: CellCoord) {
        let (viewport_width, viewport_height) = self.scrollable_viewport;
        let (row_top, row_height, col_left, col_width) = {
            let layout = self.active_layout();
            (
                layout.scrollable_row_top(coord.row),
                layout.row_height(coord.row),
                layout.columns_width(layout.pinned_columns, coord.col),
                layout.column_width(coord.col),
            )
        };

        let target_v = row_top - viewport_height / 2.0 + row_height / 2.0;
        self.set_vertical_offset(target_v, viewport_height);

        let target_h = (col_left - viewport_width / 2.0 + col_width / 2.0).max(0.0);
        let max_x = self.horizontal_scroll.max_offset().width;
        let next_x = px(target_h).min(max_x).max(px(0.0));
        let current_y = self.horizontal_scroll.offset().y;
        self.horizontal_scroll.set_offset(point(-next_x, current_y));
    }

    fn start_column_resize(&mut self, col_ix: usize, pointer_x: Pixels) {
        let columns = self.resize_columns_for_drag(col_ix);
        let start_widths = columns
            .iter()
            .map(|col_ix| self.active_layout().column_width(*col_ix))
            .collect();
        self.selection_drag = None;
        self.resize_drag = Some(ResizeDrag::Columns {
            start_x: pointer_x,
            columns,
            start_widths,
        });
        self.show_summary_menu = false;
    }

    fn start_row_resize(&mut self, row_ix: usize, pointer_y: Pixels) {
        let rows = self.resize_rows_for_drag(row_ix);
        let start_heights = rows
            .iter()
            .map(|row_ix| self.active_layout().row_height(*row_ix))
            .collect();
        self.selection_drag = None;
        self.resize_drag = Some(ResizeDrag::Rows {
            start_y: pointer_y,
            rows,
            start_heights,
        });
        self.show_summary_menu = false;
    }

    fn drag_resize(&mut self, position: Point<Pixels>) -> bool {
        let Some(resize_drag) = self.resize_drag.clone() else {
            return false;
        };

        match resize_drag {
            ResizeDrag::Columns {
                start_x,
                columns,
                start_widths,
            } => {
                let delta = f32::from(position.x - start_x);
                let updates = columns
                    .into_iter()
                    .zip(start_widths)
                    .map(|(col_ix, start_width)| {
                        (col_ix, (start_width + delta).max(MIN_COLUMN_WIDTH))
                    })
                    .collect::<Vec<_>>();
                self.active_layout_mut().set_column_widths(&updates);
            }
            ResizeDrag::Rows {
                start_y,
                rows,
                start_heights,
            } => {
                let delta = f32::from(position.y - start_y);
                let updates = rows
                    .into_iter()
                    .zip(start_heights)
                    .map(|(row_ix, start_height)| {
                        (row_ix, (start_height + delta).max(MIN_ROW_HEIGHT))
                    })
                    .collect::<Vec<_>>();
                self.active_layout_mut().set_row_heights(&updates);
            }
        }

        true
    }

    fn end_resize(&mut self) {
        self.resize_drag = None;
    }

    fn start_column_freeze_drag(&mut self, pointer_x: Pixels) {
        let axis_origin_x = pointer_x - px(self.active_layout().pinned_column_width());
        self.selection_drag = None;
        self.resize_drag = None;
        self.freeze_drag = Some(FreezeDrag::Columns { axis_origin_x });
        self.show_summary_menu = false;
    }

    fn start_column_freeze_drag_from_zero(&mut self, pointer_x: Pixels) {
        self.selection_drag = None;
        self.resize_drag = None;
        self.freeze_drag = Some(FreezeDrag::Columns {
            axis_origin_x: pointer_x,
        });
        self.show_summary_menu = false;
    }

    fn start_row_freeze_drag(&mut self, pointer_y: Pixels) {
        let axis_origin_y = pointer_y - px(self.active_layout().pinned_row_height());
        self.selection_drag = None;
        self.resize_drag = None;
        self.freeze_drag = Some(FreezeDrag::Rows { axis_origin_y });
        self.show_summary_menu = false;
    }

    fn start_row_freeze_drag_from_zero(&mut self, pointer_y: Pixels) {
        self.selection_drag = None;
        self.resize_drag = None;
        self.freeze_drag = Some(FreezeDrag::Rows {
            axis_origin_y: pointer_y,
        });
        self.show_summary_menu = false;
    }

    fn drag_freeze(&mut self, position: Point<Pixels>) -> bool {
        let Some(freeze_drag) = self.freeze_drag else {
            return false;
        };

        match freeze_drag {
            FreezeDrag::Columns { axis_origin_x } => {
                let target = freeze_target_column(
                    self.active_layout(),
                    f32::from(position.x - axis_origin_x),
                );
                self.active_layout_mut().set_pinned_columns(target)
            }
            FreezeDrag::Rows { axis_origin_y } => {
                let target =
                    freeze_target_row(self.active_layout(), f32::from(position.y - axis_origin_y));
                if self.active_layout_mut().set_pinned_rows(target) {
                    self.vertical_offset = 0.0;
                    true
                } else {
                    false
                }
            }
        }
    }

    fn end_freeze_drag(&mut self) {
        self.freeze_drag = None;
    }

    fn resize_columns_for_drag(&self, col_ix: usize) -> Vec<usize> {
        if self.selection.range.intersects_col(col_ix) {
            let range = self.selection.range.normalized();
            (range.start.col..=range.end.col)
                .filter(|col_ix| *col_ix < self.active_sheet().col_count())
                .collect()
        } else {
            vec![col_ix]
        }
    }

    fn resize_rows_for_drag(&self, row_ix: usize) -> Vec<usize> {
        if self.selection.range.intersects_row(row_ix) {
            let range = self.selection.range.normalized();
            (range.start.row..=range.end.row)
                .filter(|row_ix| *row_ix < self.active_sheet().row_count())
                .collect()
        } else {
            vec![row_ix]
        }
    }

    /// The scroll position to draw the thumb at: the live drag preview while
    /// the scrollbar is being dragged, otherwise the committed offset.
    fn vertical_scroll_position(&self) -> Pixels {
        if let Some(ScrollbarDrag::Vertical {
            scroll_position, ..
        }) = self.scrollbar_drag
        {
            return scroll_position;
        }

        px(self.vertical_offset)
    }

    /// Largest valid scroll offset so the last content stays at the viewport
    /// bottom rather than scrolling past it.
    fn max_vertical_offset(&self, viewport_height: f32) -> f32 {
        (self.active_layout().scrollable_height() - viewport_height).max(0.0)
    }

    fn set_vertical_offset(&mut self, offset: f32, viewport_height: f32) {
        self.vertical_offset = offset.clamp(0.0, self.max_vertical_offset(viewport_height));
    }
}

impl Focusable for SpreadsheetViewer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SpreadsheetViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let workbook = Arc::clone(&self.workbook);
        let sheet_ix = self.active_sheet;
        let selection = self.selection;
        let layout = self.active_layout().clone();
        let formula_height = formula_bar_height_for_sheet(workbook.sheet(sheet_ix), selection);
        let window_size = window.bounds().size;
        let scrollbars = scrollbar_visibility_for_window_size(
            &layout,
            formula_height,
            f32::from(window_size.width),
            f32::from(window_size.height),
        );
        if !scrollbars.horizontal {
            let offset = self.horizontal_scroll.offset();
            if offset.x != px(0.0) {
                self.horizontal_scroll.set_offset(point(px(0.0), offset.y));
            }
        }
        let body_width = (f32::from(window_size.width)
            - layout.row_header_width
            - if scrollbars.vertical {
                SCROLLBAR_SIZE
            } else {
                0.0
            })
        .max(0.0);
        let scrollable_column_viewport_width = (body_width - layout.pinned_column_width()).max(0.0);
        let body_height = (f32::from(window_size.height)
            - TITLE_BAR_HEIGHT
            - formula_height
            - HEADER_HEIGHT
            - FOOTER_HEIGHT
            - if scrollbars.horizontal {
                SCROLLBAR_SIZE
            } else {
                0.0
            })
        .max(0.0);
        let scrollable_row_viewport_height = (body_height - layout.pinned_row_height()).max(0.0);
        // Window resizes can shrink content below the current offset; keep the
        // committed offset valid so the viewport never shows past the end.
        self.vertical_offset = self
            .vertical_offset
            .min(self.max_vertical_offset(scrollable_row_viewport_height));
        let vertical_offset = self.vertical_offset;
        self.scrollable_viewport = (
            scrollable_column_viewport_width,
            scrollable_row_viewport_height,
        );
        // Clicking away from the name box blurs it; drop the edit buffer so the
        // box falls back to showing the current selection address.
        if self.name_box_input.is_some() && !self.name_box_focus.is_focused(window) {
            self.name_box_input = None;
            self.name_box_select_all = false;
        }
        let entity = cx.entity();
        let focus_handle = self.focus_handle.clone();

        div()
            .id("spreadsheet-viewport")
            .size_full()
            .key_context("SpreadsheetViewer")
            .track_focus(&self.focus_handle(cx))
            .on_action(cx.listener(Self::copy_selection))
            .on_action(cx.listener(Self::close_file))
            .bg(rgb(SHEET_BG))
            .text_color(rgb(CELL_TEXT))
            .text_size(px(12.0))
            .font_family("Arial")
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, move |_, window, _| {
                focus_handle.focus(window);
            })
            .on_mouse_up(MouseButton::Left, move |_, _, cx| {
                entity.update(cx, |viewer, _| {
                    viewer.selection_drag = None;
                    viewer.end_resize();
                    viewer.end_freeze_drag();
                });
            })
            .on_mouse_move({
                let entity = cx.entity();
                move |event, _, cx| {
                    if !event.dragging() {
                        return;
                    }

                    entity.update(cx, |viewer, cx| {
                        if viewer.drag_resize(event.position) || viewer.drag_freeze(event.position)
                        {
                            cx.notify();
                        }
                    });
                }
            })
            .child(title_bar(workbook.as_ref(), sheet_ix))
            .child(formula_bar(
                workbook.sheet(sheet_ix),
                selection,
                self.name_box_input.clone(),
                self.name_box_select_all,
                &self.name_box_focus,
                &self.focus_handle,
                &cx.entity(),
            ))
            .child(
                div()
                    .flex()
                    .h(px(HEADER_HEIGHT))
                    .flex_none()
                    .child(corner_header(&layout, cx.entity()))
                    .child(column_header_pane(
                        &workbook,
                        sheet_ix,
                        &layout,
                        selection,
                        &self.horizontal_scroll,
                        scrollable_column_viewport_width,
                        &cx.entity(),
                    ))
                    .when(scrollbars.vertical, |element| {
                        element.child(
                            div()
                                .w(px(SCROLLBAR_SIZE))
                                .h_full()
                                .flex_none()
                                .bg(rgb(HEADER_BG)),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .child(body_pane(
                        &workbook,
                        sheet_ix,
                        &layout,
                        selection,
                        &self.horizontal_scroll,
                        scrollable_column_viewport_width,
                        scrollable_row_viewport_height,
                        vertical_offset,
                        &cx.entity(),
                        window,
                    ))
                    .when(scrollbars.vertical, |element| {
                        element.child(vertical_scrollbar(self, scrollable_row_viewport_height, cx))
                    }),
            )
            .when(scrollbars.horizontal, |element| {
                element.child(
                    div()
                        .flex()
                        .h(px(SCROLLBAR_SIZE))
                        .flex_none()
                        .child(div().w(px(layout.row_header_width)).h_full().flex_none())
                        .when(layout.pinned_columns > 0, |element| {
                            element.child(
                                div()
                                    .w(px(layout.pinned_column_width()))
                                    .h_full()
                                    .flex_none()
                                    .bg(rgb(HEADER_BG)),
                            )
                        })
                        .child(horizontal_scrollbar(self, cx))
                        .when(scrollbars.vertical, |element| {
                            element.child(
                                div()
                                    .w(px(SCROLLBAR_SIZE))
                                    .h_full()
                                    .flex_none()
                                    .bg(rgb(HEADER_BG)),
                            )
                        }),
                )
            })
            .child(footer(self, cx))
    }
}

fn scrollbar_visibility_for_window_size(
    layout: &SheetLayout,
    formula_height: f32,
    window_width: f32,
    window_height: f32,
) -> ScrollbarVisibility {
    let body_width = (window_width - layout.row_header_width).max(0.0);
    let body_height =
        (window_height - TITLE_BAR_HEIGHT - formula_height - HEADER_HEIGHT - FOOTER_HEIGHT)
            .max(0.0);
    let pinned_width = layout.pinned_column_width();
    let pinned_height = layout.pinned_row_height();
    let scrollable_width = layout.scrollable_width();
    let scrollable_height = layout.scrollable_height();

    let mut visibility = ScrollbarVisibility {
        horizontal: scrollable_width > (body_width - pinned_width).max(0.0),
        vertical: scrollable_height > (body_height - pinned_height).max(0.0),
    };

    for _ in 0..2 {
        let available_width = if visibility.vertical {
            (body_width - SCROLLBAR_SIZE).max(0.0)
        } else {
            body_width
        };
        let available_height = if visibility.horizontal {
            (body_height - SCROLLBAR_SIZE).max(0.0)
        } else {
            body_height
        };
        let next = ScrollbarVisibility {
            horizontal: scrollable_width > (available_width - pinned_width).max(0.0),
            vertical: scrollable_height > (available_height - pinned_height).max(0.0),
        };
        if next == visibility {
            break;
        }
        visibility = next;
    }

    visibility
}

fn title_bar(workbook: &WorkbookData, sheet_ix: usize) -> Div {
    div()
        .h(px(TITLE_BAR_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .pl(px(78.0))
        .pr(px(12.0))
        .bg(rgb(TITLE_BAR_BG))
        .border_b_1()
        .border_color(rgb(GRID_COLOR))
        .text_color(rgb(HEADER_TEXT))
        .on_mouse_down(MouseButton::Left, |_, window, _| {
            window.start_window_move();
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .child(workbook.display_name()),
                )
                .child(div().text_color(rgb(FORMULA_FALLBACK_TEXT)).child("/"))
                .child(workbook.sheet_name(sheet_ix).to_owned()),
        )
}

fn column_header_pane(
    workbook: &Arc<WorkbookData>,
    sheet_ix: usize,
    layout: &SheetLayout,
    selection: Selection,
    horizontal_scroll: &ScrollHandle,
    scrollable_viewport_width: f32,
    entity: &Entity<SpreadsheetViewer>,
) -> AnyElement {
    let workbook = Arc::clone(workbook);
    let horizontal_scroll = horizontal_scroll.clone();
    let scroll_entity: Entity<SpreadsheetViewer> = (*entity).clone();
    let header_entity: Entity<SpreadsheetViewer> = (*entity).clone();
    let freeze_entity: Entity<SpreadsheetViewer> = (*entity).clone();
    let horizontal_offset = horizontal_scroll.offset().x;
    let pinned_width = layout.pinned_column_width();
    let scrollable_width = layout.scrollable_width();
    let scrollable_columns = layout
        .visible_scrollable_column_range(-f32::from(horizontal_offset), scrollable_viewport_width);
    let scrollable_columns_left =
        layout.columns_width(layout.pinned_columns, scrollable_columns.start);

    div()
        .id("column-header")
        .flex_1()
        .h_full()
        .flex()
        .overflow_hidden()
        .when(layout.pinned_columns > 0, |element| {
            element.child(
                div()
                    .relative()
                    .h_full()
                    .w(px(pinned_width))
                    .flex_none()
                    .overflow_hidden()
                    .child(column_headers_range(
                        &workbook,
                        sheet_ix,
                        layout,
                        selection,
                        &header_entity,
                        0,
                        layout.pinned_columns,
                    ))
                    .child(vertical_freeze_line(freeze_entity.clone())),
            )
        })
        .child(restrict_scroll_to_axis(
            div()
                .id("column-header-scroll")
                .flex_1()
                .h_full()
                .overflow_hidden()
                .track_scroll(&horizontal_scroll)
                .on_scroll_wheel(horizontal_scroll_handler(horizontal_scroll, scroll_entity))
                .child(
                    div().relative().h_full().w(px(scrollable_width)).child(
                        column_headers_range(
                            &workbook,
                            sheet_ix,
                            layout,
                            selection,
                            &header_entity,
                            scrollable_columns.start,
                            scrollable_columns.end,
                        )
                        .absolute()
                        .top(px(0.0))
                        .left(px(scrollable_columns_left)),
                    ),
                ),
        ))
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn body_pane(
    workbook: &Arc<WorkbookData>,
    sheet_ix: usize,
    layout: &SheetLayout,
    selection: Selection,
    horizontal_scroll: &ScrollHandle,
    scrollable_viewport_width: f32,
    scrollable_viewport_height: f32,
    vertical_offset: f32,
    entity: &Entity<SpreadsheetViewer>,
    window: &mut Window,
) -> AnyElement {
    let workbook = Arc::clone(workbook);
    let horizontal_scroll = horizontal_scroll.clone();
    let pinned_horizontal_scroll = horizontal_scroll.clone();
    let row_horizontal_offset = horizontal_scroll.offset().x;
    let scroll_entity: Entity<SpreadsheetViewer> = (*entity).clone();
    let wheel_entity: Entity<SpreadsheetViewer> = (*entity).clone();
    let row_entity: Entity<SpreadsheetViewer> = (*entity).clone();
    let pinned_entity: Entity<SpreadsheetViewer> = (*entity).clone();
    let pinned_rows_height = layout.pinned_row_height();
    let pinned_rows = layout.pinned_rows;
    let scrollable_columns = layout.visible_scrollable_column_range(
        -f32::from(horizontal_scroll.offset().x),
        scrollable_viewport_width,
    );
    let scrollable_rows =
        layout.visible_scrollable_row_range(vertical_offset, scrollable_viewport_height);
    let content_height = layout.scrollable_height();
    let visible_top = layout.scrollable_row_top(scrollable_rows.start);
    let max_vertical_offset = (content_height - scrollable_viewport_height).max(0.0);

    // Place the rendered window at its true position minus the scroll offset.
    // Only the visible rows exist as elements; there is no full-height spacer
    // (a ~10^9px element would blow out the parent flex layout and collapse the
    // scrollbar), so virtualization stays O(visible) and the layout stays sane.
    let mut visible = div()
        .absolute()
        .left(px(0.0))
        .top(px(visible_top - vertical_offset))
        .w_full()
        .flex()
        .flex_col();
    for row_ix in scrollable_rows.clone() {
        visible = visible.child(render_body_row(
            workbook.sheet(sheet_ix),
            layout,
            row_ix,
            row_horizontal_offset,
            scrollable_columns.clone(),
            selection,
            &row_entity,
            window,
        ));
    }

    div()
        .id("sheet-body")
        .flex_1()
        .h_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .on_scroll_wheel(body_scroll_wheel_handler(
            horizontal_scroll,
            scroll_entity,
            wheel_entity,
            max_vertical_offset,
        ))
        .when(pinned_rows > 0, |element| {
            let mut pinned = div()
                .relative()
                .w_full()
                .h(px(pinned_rows_height))
                .flex()
                .flex_col()
                .flex_none()
                .overflow_hidden();

            for row_ix in 0..pinned_rows {
                pinned = pinned.child(render_body_row(
                    workbook.sheet(sheet_ix),
                    layout,
                    row_ix,
                    pinned_horizontal_scroll.offset().x,
                    scrollable_columns.clone(),
                    selection,
                    &pinned_entity,
                    window,
                ));
            }

            element.child(pinned.child(horizontal_freeze_line(pinned_entity.clone())))
        })
        .child(
            div()
                .flex_1()
                .size_full()
                .relative()
                .overflow_hidden()
                .child(visible),
        )
        .into_any_element()
}

fn horizontal_scroll_handler(
    horizontal_scroll: ScrollHandle,
    entity: Entity<SpreadsheetViewer>,
) -> impl Fn(&gpui::ScrollWheelEvent, &mut Window, &mut App) + 'static {
    move |event, window, cx| {
        let delta = event.delta.pixel_delta(window.line_height());
        if delta.x.abs() <= delta.y.abs() {
            return;
        }

        let current = horizontal_scroll.offset();
        let max_offset = horizontal_scroll.max_offset().width;
        let next_x = (current.x + delta.x).clamp(-max_offset, px(0.0));
        horizontal_scroll.set_offset(point(next_x, current.y));
        cx.notify(entity.entity_id());
        cx.stop_propagation();
    }
}

/// Wheel handler for the body: horizontal wheel drives the shared horizontal
/// `ScrollHandle` (kept in sync with the column header), vertical wheel drives
/// the manually virtualized vertical offset on the viewer.
fn body_scroll_wheel_handler(
    horizontal_scroll: ScrollHandle,
    horizontal_entity: Entity<SpreadsheetViewer>,
    vertical_entity: Entity<SpreadsheetViewer>,
    max_vertical_offset: f32,
) -> impl Fn(&gpui::ScrollWheelEvent, &mut Window, &mut App) + 'static {
    move |event, window, cx| {
        let delta = event.delta.pixel_delta(window.line_height());
        if delta.x.abs() > delta.y.abs() {
            let current = horizontal_scroll.offset();
            let max_offset = horizontal_scroll.max_offset().width;
            let next_x = (current.x + delta.x).clamp(-max_offset, px(0.0));
            horizontal_scroll.set_offset(point(next_x, current.y));
            cx.notify(horizontal_entity.entity_id());
            cx.stop_propagation();
            return;
        }

        if delta.y == px(0.0) {
            return;
        }

        // GPUI reports downward wheel motion as negative; the content moves up,
        // so the scroll offset grows.
        let step = -f32::from(delta.y);
        vertical_entity.update(cx, |viewer, _| {
            viewer.vertical_offset =
                (viewer.vertical_offset + step).clamp(0.0, max_vertical_offset);
        });
        cx.notify(vertical_entity.entity_id());
        cx.stop_propagation();
    }
}

fn restrict_scroll_to_axis<E: Styled>(mut element: E) -> E {
    element.style().restrict_scroll_to_axis = Some(true);
    element
}

fn horizontal_scrollbar(
    viewer: &mut SpreadsheetViewer,
    cx: &mut Context<'_, SpreadsheetViewer>,
) -> AnyElement {
    let handle = viewer.horizontal_scroll.clone();
    let content_width = px(viewer.active_layout().scrollable_width());

    scrollbar_track("horizontal-scrollbar-track")
        .flex_1()
        .child(scrollbar_thumb(
            ScrollbarAxis::Horizontal,
            handle.offset().x,
            content_width,
            handle.clone(),
            cx,
        ))
        .into_any_element()
}

fn vertical_scrollbar(
    viewer: &mut SpreadsheetViewer,
    viewport_height: f32,
    cx: &mut Context<'_, SpreadsheetViewer>,
) -> AnyElement {
    let content_height = px(viewer.active_layout().scrollable_height()).max(px(viewport_height));
    let scroll_position = viewer.vertical_scroll_position();

    scrollbar_track("vertical-scrollbar-track")
        .w(px(SCROLLBAR_SIZE))
        .h_full()
        .flex_none()
        .child(vertical_scrollbar_thumb(
            scroll_position,
            content_height,
            viewport_height,
            cx,
        ))
        .into_any_element()
}

fn scrollbar_track(id: impl Into<gpui::ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .relative()
        .bg(rgb(0xf1_f3_f4))
        .border_1()
        .border_color(rgb(GRID_COLOR))
}

#[derive(Clone, Copy)]
enum ScrollbarAxis {
    Horizontal,
    Vertical,
}

fn scrollbar_thumb(
    axis: ScrollbarAxis,
    scroll_offset: Pixels,
    content_size: Pixels,
    scroll_handle: ScrollHandle,
    cx: &mut Context<'_, SpreadsheetViewer>,
) -> AnyElement {
    if content_size <= px(0.0) {
        return div().into_any_element();
    }

    let entity = cx.entity();
    let scroll_position = -scroll_offset;

    canvas(
        |_, _, _| (),
        move |track_bounds, (), window, _| {
            let viewport_size = track_size(axis, track_bounds);
            let max_offset = (content_size - viewport_size).max(px(0.0));
            if max_offset <= px(0.0) || viewport_size <= px(0.0) {
                return;
            }

            let thumb_bounds = thumb_bounds(
                axis,
                track_bounds,
                viewport_size,
                max_offset,
                scroll_position,
            );

            window.paint_quad(gpui::fill(thumb_bounds, rgb(0xc0_c0_c0)));

            window.on_mouse_event({
                let entity = entity.clone();
                move |event: &MouseDownEvent, _, _, cx| {
                    if !thumb_bounds.contains(&event.position) {
                        return;
                    }

                    let pointer_offset = match axis {
                        ScrollbarAxis::Horizontal => event.position.x - thumb_bounds.origin.x,
                        ScrollbarAxis::Vertical => event.position.y - thumb_bounds.origin.y,
                    };

                    entity.update(cx, |viewer, _| {
                        viewer.scrollbar_drag = Some(match axis {
                            ScrollbarAxis::Horizontal => {
                                ScrollbarDrag::Horizontal { pointer_offset }
                            }
                            ScrollbarAxis::Vertical => ScrollbarDrag::Vertical {
                                pointer_offset,
                                scroll_position,
                                last_sheet_update: Instant::now(),
                            },
                        });
                    });
                }
            });

            window.on_mouse_event({
                let entity = entity.clone();
                move |_: &MouseUpEvent, _, _, cx| {
                    entity.update(cx, |viewer, _| {
                        viewer.scrollbar_drag = None;
                    });
                }
            });

            window.on_mouse_event({
                let entity = entity.clone();
                let scroll_handle = scroll_handle.clone();
                move |event: &MouseMoveEvent, _, _, cx| {
                    if !event.dragging() {
                        return;
                    }

                    let Some(pointer_offset) = entity.read(cx).drag_pointer_offset(axis) else {
                        return;
                    };

                    let scroll_position = scrollbar_position_to_scroll_offset(
                        axis,
                        track_bounds,
                        thumb_bounds,
                        event.position,
                        pointer_offset,
                        max_offset,
                    );
                    let current = scroll_handle.offset();
                    let next = match axis {
                        ScrollbarAxis::Horizontal => point(-scroll_position, current.y),
                        ScrollbarAxis::Vertical => point(current.x, -scroll_position),
                    };
                    scroll_handle.set_offset(next);
                    cx.notify(entity.entity_id());
                }
            });
        },
    )
    .size_full()
    .into_any_element()
}

fn vertical_scrollbar_thumb(
    scroll_position: Pixels,
    content_size: Pixels,
    viewport_height: f32,
    cx: &mut Context<'_, SpreadsheetViewer>,
) -> AnyElement {
    if content_size <= px(0.0) {
        return div().into_any_element();
    }

    let entity = cx.entity();

    canvas(
        |_, _, _| (),
        move |track_bounds, (), window, _| {
            let axis = ScrollbarAxis::Vertical;
            let viewport_size = track_size(axis, track_bounds);
            let max_offset = (content_size - viewport_size).max(px(0.0));
            if max_offset <= px(0.0) || viewport_size <= px(0.0) {
                return;
            }

            let thumb_bounds = thumb_bounds(
                axis,
                track_bounds,
                viewport_size,
                max_offset,
                scroll_position,
            );

            window.paint_quad(gpui::fill(thumb_bounds, rgb(0xc0_c0_c0)));

            window.on_mouse_event({
                let entity = entity.clone();
                move |event: &MouseDownEvent, _, _, cx| {
                    if !thumb_bounds.contains(&event.position) {
                        return;
                    }

                    entity.update(cx, |viewer, _| {
                        viewer.scrollbar_drag = Some(ScrollbarDrag::Vertical {
                            pointer_offset: event.position.y - thumb_bounds.origin.y,
                            scroll_position,
                            last_sheet_update: Instant::now(),
                        });
                    });
                }
            });

            window.on_mouse_event({
                let entity = entity.clone();
                move |_: &MouseUpEvent, _, _, cx| {
                    entity.update(cx, |viewer, _| {
                        if let Some(ScrollbarDrag::Vertical {
                            scroll_position, ..
                        }) = viewer.scrollbar_drag
                        {
                            viewer.set_vertical_offset(f32::from(scroll_position), viewport_height);
                        }
                        viewer.scrollbar_drag = None;
                    });
                    cx.notify(entity.entity_id());
                }
            });

            window.on_mouse_event({
                let entity = entity.clone();
                move |event: &MouseMoveEvent, _, _, cx| {
                    if !event.dragging() {
                        return;
                    }

                    let Some(pointer_offset) =
                        entity.read(cx).drag_pointer_offset(ScrollbarAxis::Vertical)
                    else {
                        return;
                    };

                    let scroll_position = scrollbar_position_to_scroll_offset(
                        axis,
                        track_bounds,
                        thumb_bounds,
                        event.position,
                        pointer_offset,
                        max_offset,
                    );

                    entity.update(cx, |viewer, _| {
                        let now = Instant::now();
                        let last_sheet_update = match viewer.scrollbar_drag {
                            Some(ScrollbarDrag::Vertical {
                                last_sheet_update, ..
                            }) => last_sheet_update,
                            _ => now,
                        };
                        let update_interval = vertical_scroll_drag_update_interval(
                            viewer.active_sheet().is_fully_loaded(),
                        );
                        let should_update_sheet =
                            now.duration_since(last_sheet_update) >= update_interval;

                        let last_sheet_update = if should_update_sheet {
                            viewer.set_vertical_offset(f32::from(scroll_position), viewport_height);
                            now
                        } else {
                            last_sheet_update
                        };

                        viewer.scrollbar_drag = Some(ScrollbarDrag::Vertical {
                            pointer_offset,
                            scroll_position,
                            last_sheet_update,
                        });
                    });
                    cx.notify(entity.entity_id());
                }
            });
        },
    )
    .size_full()
    .into_any_element()
}

impl SpreadsheetViewer {
    fn drag_pointer_offset(&self, axis: ScrollbarAxis) -> Option<Pixels> {
        match (axis, self.scrollbar_drag) {
            (ScrollbarAxis::Horizontal, Some(ScrollbarDrag::Horizontal { pointer_offset }))
            | (ScrollbarAxis::Vertical, Some(ScrollbarDrag::Vertical { pointer_offset, .. })) => {
                Some(pointer_offset)
            }
            _ => None,
        }
    }
}

fn track_size(axis: ScrollbarAxis, track_bounds: Bounds<Pixels>) -> Pixels {
    match axis {
        ScrollbarAxis::Horizontal => track_bounds.size.width,
        ScrollbarAxis::Vertical => track_bounds.size.height,
    }
}

fn thumb_bounds(
    axis: ScrollbarAxis,
    track_bounds: Bounds<Pixels>,
    viewport_size: Pixels,
    max_offset: Pixels,
    scroll_position: Pixels,
) -> Bounds<Pixels> {
    let track_size = track_size(axis, track_bounds);
    let content_size = viewport_size + max_offset;
    let thumb_size = (track_size * (viewport_size / content_size)).max(px(MIN_THUMB_SIZE));
    let travel = (track_size - thumb_size).max(px(0.0));
    let thumb_start = if max_offset > px(0.0) {
        travel * (scroll_position / max_offset)
    } else {
        px(0.0)
    }
    .clamp(px(0.0), travel);

    match axis {
        ScrollbarAxis::Horizontal => Bounds {
            origin: point(track_bounds.origin.x + thumb_start, track_bounds.origin.y),
            size: gpui::size(thumb_size, track_bounds.size.height),
        },
        ScrollbarAxis::Vertical => Bounds {
            origin: point(track_bounds.origin.x, track_bounds.origin.y + thumb_start),
            size: gpui::size(track_bounds.size.width, thumb_size),
        },
    }
}

fn scrollbar_position_to_scroll_offset(
    axis: ScrollbarAxis,
    track_bounds: Bounds<Pixels>,
    thumb_bounds: Bounds<Pixels>,
    pointer_position: Point<Pixels>,
    pointer_offset: Pixels,
    max_offset: Pixels,
) -> Pixels {
    let (track_origin, track_size, thumb_size, pointer_position) = match axis {
        ScrollbarAxis::Horizontal => (
            track_bounds.origin.x,
            track_bounds.size.width,
            thumb_bounds.size.width,
            pointer_position.x,
        ),
        ScrollbarAxis::Vertical => (
            track_bounds.origin.y,
            track_bounds.size.height,
            thumb_bounds.size.height,
            pointer_position.y,
        ),
    };
    let travel = (track_size - thumb_size).max(px(0.0));
    if travel <= px(0.0) {
        return px(0.0);
    }

    let thumb_start = (pointer_position - track_origin - pointer_offset).clamp(px(0.0), travel);
    max_offset * (thumb_start / travel)
}

#[allow(clippy::too_many_arguments)]
fn formula_bar(
    sheet: &SheetData,
    selection: Selection,
    name_box_input: Option<String>,
    name_box_select_all: bool,
    name_box_focus: &FocusHandle,
    grid_focus: &FocusHandle,
    entity: &Entity<SpreadsheetViewer>,
) -> Div {
    let formula_value = formula_bar_value(sheet, selection);
    let multiline = formula_value.contains('\n');
    let height = formula_bar_height(&formula_value);
    let field_height = (height - FORMULA_BAR_VERTICAL_PADDING).max(24.0);

    div()
        .h(px(height))
        .flex_none()
        .flex()
        .items_start()
        .gap_2()
        .py_1()
        .px_2()
        .bg(rgb(HEADER_BG))
        .border_b_1()
        .border_color(rgb(GRID_COLOR))
        .child(name_box(
            name_box_label(selection, sheet),
            name_box_input,
            name_box_select_all,
            name_box_focus,
            grid_focus,
            entity,
        ))
        .child(
            div()
                .flex_1()
                .h(px(field_height))
                .flex()
                .items_start()
                .px_2()
                .overflow_hidden()
                .bg(rgb(CELL_BG))
                .border_1()
                .border_color(rgb(GRID_COLOR))
                .when(!multiline, |element| {
                    element.items_center().whitespace_nowrap()
                })
                .when(multiline, Styled::whitespace_normal)
                .child(formula_value),
        )
}

fn name_box(
    selection_label: String,
    name_box_input: Option<String>,
    name_box_select_all: bool,
    name_box_focus: &FocusHandle,
    grid_focus: &FocusHandle,
    entity: &Entity<SpreadsheetViewer>,
) -> Stateful<Div> {
    let editing = name_box_input.is_some();
    let select_all = editing && name_box_select_all;
    let current_address = selection_label.clone();
    let text = name_box_input.unwrap_or(selection_label);

    let mut container = div()
        .id("name-box")
        .track_focus(name_box_focus)
        .w(px(72.0))
        .h(px(24.0))
        .mt(px(1.0))
        .flex()
        .items_center()
        .justify_center()
        .bg(rgb(CELL_BG))
        .border_1()
        .border_color(rgb(if editing {
            SELECTION_BORDER
        } else {
            GRID_COLOR
        }))
        .text_color(rgb(HEADER_TEXT))
        .cursor(CursorStyle::IBeam)
        .on_mouse_down(MouseButton::Left, {
            let name_box_focus = name_box_focus.clone();
            let entity = entity.clone();
            let current_address = current_address.clone();
            move |event: &MouseDownEvent, window, cx| {
                name_box_focus.focus(window);
                let select_all = event.click_count >= 2;
                entity.update(cx, |viewer, cx| {
                    if select_all {
                        viewer.name_box_input = Some(current_address.clone());
                        viewer.name_box_select_all = true;
                        cx.notify();
                    } else if viewer.name_box_input.is_none() {
                        viewer.name_box_input = Some(current_address.clone());
                        viewer.name_box_select_all = false;
                        cx.notify();
                    } else if viewer.name_box_select_all {
                        // Single click while the value is selected clears the
                        // selection, like other text inputs.
                        viewer.name_box_select_all = false;
                        cx.notify();
                    }
                });
                cx.stop_propagation();
            }
        })
        .on_key_down({
            let entity = entity.clone();
            let grid_focus = grid_focus.clone();
            move |event, window, cx| {
                if name_box_key_down(&entity, event, cx) {
                    grid_focus.focus(window);
                }
                cx.stop_propagation();
            }
        });

    if select_all {
        container = container.child(
            div()
                .px_1()
                .bg(rgb(SELECTION_INNER_BORDER))
                .text_color(rgb(CELL_TEXT))
                .child(text),
        );
    } else {
        container = container.child(text);
        if editing {
            container = container.child(div().w(px(1.0)).h(px(14.0)).bg(rgb(SELECTION_BORDER)));
        }
    }

    container
}

/// Applies a name box keystroke. Returns `true` when the box should be closed
/// and focus returned to the grid (Enter committed, or Escape cancelled).
fn name_box_key_down(
    entity: &Entity<SpreadsheetViewer>,
    event: &KeyDownEvent,
    cx: &mut App,
) -> bool {
    let key_char = event.keystroke.key_char.clone();
    entity.update(cx, |viewer, cx| {
        if viewer.name_box_input.is_none() {
            return false;
        }

        match event.keystroke.key.as_str() {
            "escape" => {
                viewer.name_box_input = None;
                cx.notify();
                true
            }
            "enter" => {
                let text = viewer.name_box_input.take().unwrap_or_default();
                let sheet = viewer.active_sheet();
                let parsed = parse_name_box_reference(&text, sheet.col_count(), sheet.row_count());
                if let Some(name_ref) = parsed {
                    viewer.apply_name_ref(name_ref);
                }
                cx.notify();
                true
            }
            "backspace" => {
                if let Some(buffer) = viewer.name_box_input.as_mut() {
                    if viewer.name_box_select_all {
                        buffer.clear();
                    } else {
                        buffer.pop();
                    }
                }
                viewer.name_box_select_all = false;
                cx.notify();
                false
            }
            _ => {
                if let Some(text) = key_char
                    && !text.is_empty()
                    && !text.chars().any(char::is_control)
                    && let Some(buffer) = viewer.name_box_input.as_mut()
                {
                    if viewer.name_box_select_all {
                        buffer.clear();
                    }
                    buffer.push_str(&text);
                    viewer.name_box_select_all = false;
                    cx.notify();
                }
                false
            }
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameRef {
    Cell(CellCoord),
    Column(usize),
    Row(usize),
}

/// Parses a name box entry: `A` selects a column, `100` selects a row, and
/// `B101` selects a cell. Out-of-range references clamp to the sheet bounds.
fn parse_name_box_reference(input: &str, col_count: usize, row_count: usize) -> Option<NameRef> {
    if col_count == 0 || row_count == 0 {
        return None;
    }

    let trimmed = input.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return None;
    }

    let upper = trimmed.to_ascii_uppercase();
    let letters: String = upper
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect();
    let digits = &upper[letters.len()..];
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    let col = if letters.is_empty() {
        None
    } else {
        Some(column_index(&letters)?.min(col_count - 1))
    };
    let row = if digits.is_empty() {
        None
    } else {
        let number: usize = digits.parse().ok()?;
        Some(number.checked_sub(1)?.min(row_count - 1))
    };

    match (col, row) {
        (Some(col), Some(row)) => Some(NameRef::Cell(CellCoord::new(row, col))),
        (Some(col), None) => Some(NameRef::Column(col)),
        (None, Some(row)) => Some(NameRef::Row(row)),
        (None, None) => None,
    }
}

/// Inverse of `column_name`: `A` -> 0, `Z` -> 25, `AA` -> 26. Returns `None`
/// only if the bijective base-26 value overflows `usize`.
fn column_index(letters: &str) -> Option<usize> {
    let mut index: usize = 0;
    for byte in letters.bytes() {
        index = index
            .checked_mul(26)?
            .checked_add(usize::from(byte - b'A' + 1))?;
    }
    index.checked_sub(1)
}

fn formula_bar_height_for_sheet(sheet: &SheetData, selection: Selection) -> f32 {
    formula_bar_height(&formula_bar_value(sheet, selection))
}

fn formula_bar_value(sheet: &SheetData, selection: Selection) -> String {
    let cell = sheet.cell_data(selection.anchor.row, selection.anchor.col);
    cell.formula.as_ref().map_or_else(
        || cell.value.clone(),
        |formula| formula_display_text(formula),
    )
}

fn formula_bar_height(value: &str) -> f32 {
    let line_count = value.split('\n').count().max(1);
    if line_count <= 1 {
        FORMULA_BAR_HEIGHT
    } else {
        (line_count as f32 * FORMULA_BAR_LINE_HEIGHT + FORMULA_BAR_VERTICAL_PADDING)
            .max(FORMULA_BAR_HEIGHT)
    }
}

fn footer(viewer: &SpreadsheetViewer, cx: &mut Context<'_, SpreadsheetViewer>) -> Div {
    let entity = cx.entity();

    div()
        .h(px(FOOTER_HEIGHT))
        .flex_none()
        .flex()
        .items_center()
        .bg(rgb(HEADER_BG))
        .border_t_1()
        .border_color(rgb(GRID_COLOR))
        .child(sheet_tabs(viewer, &entity))
        .child(summary_box(viewer, &entity))
}

fn sheet_tabs(viewer: &SpreadsheetViewer, entity: &Entity<SpreadsheetViewer>) -> Stateful<Div> {
    let mut tabs = div()
        .id("sheet-tabs")
        .flex()
        .items_center()
        .flex_1()
        .h_full()
        .overflow_x_scroll()
        .track_scroll(&viewer.tabs_scroll)
        .scrollbar_width(px(0.0));

    for sheet_ix in 0..viewer.workbook.sheet_count() {
        let selected = sheet_ix == viewer.active_sheet;
        let label = viewer.workbook.sheet_name(sheet_ix).to_owned();
        let tab_entity: Entity<SpreadsheetViewer> = (*entity).clone();
        tabs = tabs.child(
            div()
                .id(("sheet-tab", sheet_ix))
                .h_full()
                .min_w(px(72.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .px_2()
                .whitespace_nowrap()
                .cursor_pointer()
                .bg(rgb(if selected { ACTIVE_TAB_BG } else { HEADER_BG }))
                .text_color(rgb(if selected {
                    SELECTION_BORDER
                } else {
                    HEADER_TEXT
                }))
                .when(selected, |tab| {
                    tab.border_l_1().border_r_1().border_color(rgb(GRID_COLOR))
                })
                .when(!selected, |tab| {
                    tab.border_r_1()
                        .border_color(rgb(GRID_COLOR))
                        .hover(|tab| tab.bg(rgb(HOVER_CELL_BG)))
                })
                .child(label)
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                    tab_entity.update(cx, |viewer, cx| {
                        if viewer.switch_sheet(sheet_ix) {
                            cx.notify();
                        }
                    });
                }),
        );
    }

    tabs
}

fn summary_box(viewer: &SpreadsheetViewer, entity: &Entity<SpreadsheetViewer>) -> Div {
    let summary = viewer
        .active_sheet()
        .summary_for_range(viewer.selection.range);
    let metric_value = summary_metric_value(summary, viewer.summary_metric);
    let main_entity: Entity<SpreadsheetViewer> = (*entity).clone();
    let mut box_el = div()
        .h_full()
        .flex()
        .items_center()
        .gap_1()
        .px_2()
        .flex_none();

    if viewer.show_summary_menu {
        for (metric_ix, metric) in SummaryMetric::ALL.into_iter().enumerate() {
            let option_entity: Entity<SpreadsheetViewer> = (*entity).clone();
            box_el = box_el.child(
                div()
                    .id(("summary-option", metric_ix))
                    .h(px(24.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .bg(rgb(if metric == viewer.summary_metric {
                        SELECTED_CELL_BG
                    } else {
                        CELL_BG
                    }))
                    .border_1()
                    .border_color(rgb(GRID_COLOR))
                    .child(metric.label())
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        option_entity.update(cx, |viewer, cx| {
                            viewer.summary_metric = metric;
                            viewer.show_summary_menu = false;
                            cx.notify();
                        });
                    }),
            );
        }
    }

    box_el.child(
        div()
            .id("selection-summary")
            .h(px(24.0))
            .min_w(px(200.0))
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .px_2()
            .bg(rgb(CELL_BG))
            .border_1()
            .border_color(rgb(GRID_COLOR))
            .child(format!("Count: {}", summary.selected_cells))
            .child(format!(
                "{}: {}",
                viewer.summary_metric.label(),
                metric_value
            ))
            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                main_entity.update(cx, |viewer, cx| {
                    viewer.show_summary_menu = !viewer.show_summary_menu;
                    cx.notify();
                });
            }),
    )
}

fn summary_metric_value(
    summary: crate::workbook::SelectionSummary,
    metric: SummaryMetric,
) -> String {
    let value = match metric {
        SummaryMetric::Sum => Some(summary.sum),
        SummaryMetric::Mean => {
            (summary.numeric_cells > 0).then(|| summary.sum / summary.numeric_cells as f64)
        }
        SummaryMetric::Min => summary.min,
        SummaryMetric::Max => summary.max,
    };

    value.map_or_else(|| "-".to_owned(), display_summary_number)
}

fn display_summary_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

fn cell_address(coord: CellCoord) -> String {
    format!("{}{}", column_name(coord.col), coord.row + 1)
}

/// Text shown in the name box for the current selection: just the column
/// letter(s) when whole columns are selected, just the row number(s) when
/// whole rows are selected, otherwise the anchor cell address.
fn name_box_label(selection: Selection, sheet: &SheetData) -> String {
    let max_row = sheet.row_count().saturating_sub(1);
    let max_col = sheet.col_count().saturating_sub(1);
    let range = selection.range.normalized();
    let full_column = max_row > 0 && range.start.row == 0 && range.end.row == max_row;
    let full_row = max_col > 0 && range.start.col == 0 && range.end.col == max_col;

    if full_column && !full_row {
        return if range.start.col == range.end.col {
            column_name(range.start.col)
        } else {
            format!(
                "{}:{}",
                column_name(range.start.col),
                column_name(range.end.col)
            )
        };
    }
    if full_row && !full_column {
        return if range.start.row == range.end.row {
            (range.start.row + 1).to_string()
        } else {
            format!("{}:{}", range.start.row + 1, range.end.row + 1)
        };
    }

    cell_address(selection.anchor)
}

#[allow(clippy::too_many_arguments)]
fn render_body_row(
    sheet: &SheetData,
    layout: &SheetLayout,
    row_ix: usize,
    horizontal_offset: Pixels,
    scrollable_columns: Range<usize>,
    selection: Selection,
    entity: &Entity<SpreadsheetViewer>,
    window: &mut Window,
) -> AnyElement {
    let row_height = layout.row_height(row_ix);

    div()
        .id(("sheet-row", row_ix))
        .flex()
        .h(px(row_height))
        .w_full()
        .child(row_header(
            row_ix,
            row_height,
            layout.row_header_width,
            selection.range.intersects_row(row_ix),
            entity,
        ))
        .child(render_cells_row_segments(
            sheet,
            layout,
            row_ix,
            row_height,
            horizontal_offset,
            scrollable_columns,
            selection,
            entity,
            window,
        ))
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_cells_row_segments(
    sheet: &SheetData,
    layout: &SheetLayout,
    row_ix: usize,
    row_height: f32,
    horizontal_offset: Pixels,
    scrollable_columns: Range<usize>,
    selection: Selection,
    entity: &Entity<SpreadsheetViewer>,
    window: &mut Window,
) -> Div {
    let pinned_width = layout.pinned_column_width();
    let scrollable_width = layout.scrollable_width();
    let freeze_entity: Entity<SpreadsheetViewer> = (*entity).clone();
    let overlays = row_overlay_layer(
        sheet,
        layout,
        row_ix,
        row_height,
        horizontal_offset,
        &scrollable_columns,
        selection,
        entity,
        window,
    );

    div()
        .flex_1()
        .relative()
        .h(px(row_height))
        .flex()
        .overflow_hidden()
        .when(layout.pinned_columns > 0, |element| {
            element.child(
                div()
                    .relative()
                    .h(px(row_height))
                    .w(px(pinned_width))
                    .flex_none()
                    .overflow_hidden()
                    .child(render_cells_row_range(
                        sheet,
                        layout,
                        row_ix,
                        row_height,
                        selection,
                        entity,
                        window,
                        0,
                        layout.pinned_columns,
                    ))
                    .child(vertical_freeze_line(freeze_entity.clone())),
            )
        })
        .child(
            div().flex_1().h(px(row_height)).overflow_hidden().child(
                div()
                    .relative()
                    .h(px(row_height))
                    .w(px(scrollable_width))
                    .child(
                        render_cells_row_range(
                            sheet,
                            layout,
                            row_ix,
                            row_height,
                            selection,
                            entity,
                            window,
                            scrollable_columns.start,
                            scrollable_columns.end,
                        )
                        .absolute()
                        .top(px(0.0))
                        .left(px(
                            layout.columns_width(layout.pinned_columns, scrollable_columns.start)
                        )),
                    )
                    .left(horizontal_offset),
            ),
        )
        .children(overlays)
}

/// Absolutely-positioned overlays drawn on top of both panes for `row_ix`:
/// merged-cell anchors and text overflow that spills across the freeze line.
#[allow(clippy::too_many_arguments)]
fn row_overlay_layer(
    sheet: &SheetData,
    layout: &SheetLayout,
    row_ix: usize,
    row_height: f32,
    horizontal_offset: Pixels,
    scrollable_columns: &Range<usize>,
    selection: Selection,
    entity: &Entity<SpreadsheetViewer>,
    window: &mut Window,
) -> Vec<Div> {
    let mut overlays: Vec<Div> = Vec::new();
    let pinned_columns = layout.pinned_columns;
    let pinned_width = layout.pinned_column_width();

    // Absolute left edge of a column within the row-segments container.
    let column_left = |col: usize| -> f32 {
        if col < pinned_columns {
            layout.columns_width(0, col)
        } else {
            pinned_width + f32::from(horizontal_offset) + layout.columns_width(pinned_columns, col)
        }
    };

    let cell_state = |coord: CellCoord| CellRenderState {
        coord,
        selected: selection.contains(coord.row, coord.col),
        selection_edges: selection.range.edge_sides(coord.row, coord.col),
        active: selection.anchor == coord,
    };

    // Merged-cell anchors that begin on this row.
    for &region in layout.merges.regions() {
        let region = region.normalized();
        let anchor = region.start;
        if anchor.row != row_ix {
            continue;
        }
        let visible = anchor.col < pinned_columns || scrollable_columns.contains(&anchor.col);
        if !visible {
            continue;
        }
        let anchor_cell =
            build_row_cells(sheet, layout, row_ix, anchor.col, anchor.col + 1, window)
                .into_iter()
                .next();
        let Some(anchor_cell) = anchor_cell else {
            continue;
        };
        let (width, height) = layout.merged_size(region);
        overlays.push(merge_overlay_cell(
            &anchor_cell,
            column_left(anchor.col),
            width,
            height,
            cell_state(anchor),
            entity,
        ));
    }

    // Text overflow that escapes the pinned pane into the scrollable pane.
    // Only meaningful when the scrollable pane has not scrolled past its first
    // column, so the spilled-over columns are exactly those after the freeze.
    if pinned_columns > 0 && scrollable_columns.start == pinned_columns {
        let cells = build_row_cells(sheet, layout, row_ix, 0, scrollable_columns.end, window);
        let widths = overflow_text_widths_for_columns(&cells, layout, 0);
        for col_ix in 0..pinned_columns.min(cells.len()) {
            let Some(width) = widths[col_ix] else {
                continue;
            };
            let left = layout.columns_width(0, col_ix);
            // Already fits inside the pinned pane: the in-pane overlay handles it.
            if left + width <= pinned_width + 0.5 {
                continue;
            }
            overlays.push(cell_text_overlay(
                &cells[col_ix],
                left,
                layout.column_width(col_ix),
                width,
                row_height,
                cell_state(CellCoord::new(row_ix, col_ix)),
            ));
        }
    }

    overlays
}

fn merge_overlay_cell(
    row_cell: &RowCell,
    left: f32,
    width: f32,
    height: f32,
    state: CellRenderState,
    entity: &Entity<SpreadsheetViewer>,
) -> Div {
    let select_entity: Entity<SpreadsheetViewer> = (*entity).clone();
    let drag_entity = select_entity.clone();
    let cell = &row_cell.data;
    let multiline = cell.style.wrap_text || row_cell.text.contains('\n');
    let text_color = if row_cell.formula_fallback {
        FORMULA_FALLBACK_TEXT
    } else {
        cell.style.text_color.unwrap_or(CELL_TEXT)
    };
    let background = {
        let base = cell.style.background_color.unwrap_or(CELL_BG);
        if state.selected && !state.active {
            selection_tint(base)
        } else {
            base
        }
    };

    let mut element = div()
        .absolute()
        .left(px(left))
        .top(px(0.0))
        .w(px(width))
        .h(px(height))
        .flex()
        .px_2()
        .overflow_hidden()
        .border_r_1()
        .border_b_1()
        .border_color(rgb(GRID_COLOR))
        .bg(rgb(background))
        .text_color(rgb(text_color));

    element = if multiline {
        element.items_start().whitespace_normal()
    } else {
        element.items_center().whitespace_nowrap()
    };

    if cell.style.bold {
        element = element.font_weight(FontWeight::BOLD);
    }

    let mut element = element.child(row_cell.text.clone());

    if state.active || state.selected {
        element = element.child(selection_outline(state.selection_edges));
    }

    element
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            select_entity.update(cx, |viewer, cx| {
                if viewer.select_cell(state.coord, event.modifiers.shift) {
                    cx.notify();
                }
            });
        })
        .on_mouse_move(move |event, _, cx| {
            if !event.dragging() {
                return;
            }

            drag_entity.update(cx, |viewer, cx| {
                if viewer.drag_to_cell(state.coord) {
                    cx.notify();
                }
            });
        })
}

fn build_row_cells(
    sheet: &SheetData,
    layout: &SheetLayout,
    row_ix: usize,
    start_col: usize,
    end_col: usize,
    window: &mut Window,
) -> Vec<RowCell> {
    (start_col..end_col)
        .map(|col_ix| {
            let data = sheet.cell_data(row_ix, col_ix);
            let (text, formula_fallback) = cell_display_text(&data);
            let text_width = measure_cell_text_width(&data, &text, formula_fallback, window);
            RowCell {
                data,
                text,
                text_width,
                formula_fallback,
                merge: layout.merge_kind(row_ix, col_ix),
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn render_cells_row_range(
    sheet: &SheetData,
    layout: &SheetLayout,
    row_ix: usize,
    row_height: f32,
    selection: Selection,
    entity: &Entity<SpreadsheetViewer>,
    window: &mut Window,
    start_col: usize,
    end_col: usize,
) -> Stateful<Div> {
    let end_col = end_col.min(sheet.col_count());
    let width = layout.columns_width(start_col, end_col);
    let mut row = div()
        .id((
            "sheet-cells-row",
            row_ix.saturating_mul(2) + usize::from(start_col > 0),
        ))
        .flex()
        .relative()
        .h(px(row_height))
        .w(px(width));

    let cells = build_row_cells(sheet, layout, row_ix, start_col, end_col, window);
    let overflow_widths = overflow_text_widths_for_columns(&cells, layout, start_col);

    for (col_ix, row_cell) in cells.iter().enumerate() {
        let sheet_col_ix = start_col + col_ix;
        row = row.child(cell(
            row_cell,
            layout.column_width(sheet_col_ix),
            row_height,
            CellRenderState {
                coord: CellCoord::new(row_ix, sheet_col_ix),
                selected: selection.contains(row_ix, sheet_col_ix),
                selection_edges: selection.range.edge_sides(row_ix, sheet_col_ix),
                active: selection.anchor == CellCoord::new(row_ix, sheet_col_ix),
            },
            overflow_widths[col_ix].is_none(),
            entity,
        ));
    }

    let mut left = 0.0;
    for (col_ix, row_cell) in cells.iter().enumerate() {
        let sheet_col_ix = start_col + col_ix;
        if let Some(width) = overflow_widths[col_ix] {
            let state = CellRenderState {
                coord: CellCoord::new(row_ix, sheet_col_ix),
                selected: selection.contains(row_ix, sheet_col_ix),
                selection_edges: selection.range.edge_sides(row_ix, sheet_col_ix),
                active: selection.anchor == CellCoord::new(row_ix, sheet_col_ix),
            };
            row = row.child(cell_text_overlay(
                row_cell,
                left,
                layout.column_width(sheet_col_ix),
                width,
                row_height,
                state,
            ));
        }
        left += layout.column_width(sheet_col_ix);
    }

    row
}

#[cfg(test)]
fn overflow_text_widths(cells: &[RowCell], layout: &SheetLayout) -> Vec<Option<f32>> {
    overflow_text_widths_for_columns(cells, layout, 0)
}

fn overflow_text_widths_for_columns(
    cells: &[RowCell],
    layout: &SheetLayout,
    start_col: usize,
) -> Vec<Option<f32>> {
    let mut widths = vec![None; cells.len()];

    for col_ix in 0..cells.len() {
        let sheet_col_ix = start_col + col_ix;
        let cell_content_width =
            (layout.column_width(sheet_col_ix) - CELL_HORIZONTAL_PADDING).max(0.0);
        let cell = &cells[col_ix];
        if !matches!(cell.data.raw_value, CellRawValue::Text)
            || cell.formula_fallback
            || cell.data.style.wrap_text
            || cell.text.is_empty()
            || cell.merge != MergeKind::None
            || cell.text_width <= cell_content_width
        {
            continue;
        }

        // Text only spills into genuinely empty, non-merged neighbours.
        let mut end_col_ix = col_ix + 1;
        while end_col_ix < cells.len()
            && cells[end_col_ix].text.is_empty()
            && cells[end_col_ix].merge == MergeKind::None
        {
            end_col_ix += 1;
        }

        if end_col_ix <= col_ix + 1 {
            continue;
        }

        widths[col_ix] = Some(
            (col_ix..end_col_ix)
                .map(|ix| layout.column_width(start_col + ix))
                .sum(),
        );
    }

    widths
}

fn measure_cell_text_width(
    cell: &CellData,
    text: &str,
    formula_fallback: bool,
    window: &mut Window,
) -> f32 {
    if text.is_empty() || formula_fallback || cell.style.wrap_text {
        return 0.0;
    }

    let mut text_font = font("Arial");
    if cell.style.bold {
        text_font = text_font.bold();
    }
    let text_color = if formula_fallback {
        FORMULA_FALLBACK_TEXT
    } else {
        cell.style.text_color.unwrap_or(CELL_TEXT)
    };
    text.split('\n')
        .map(|line| {
            let run = TextRun {
                len: line.len(),
                font: text_font.clone(),
                color: rgb(text_color).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            f32::from(
                window
                    .text_system()
                    .layout_line(line, px(12.0), &[run], None)
                    .width,
            )
        })
        .fold(0.0, f32::max)
}

fn corner_header(layout: &SheetLayout, entity: Entity<SpreadsheetViewer>) -> Div {
    let column_freeze_entity = entity.clone();
    let row_freeze_entity = entity.clone();
    div()
        .w(px(layout.row_header_width))
        .h(px(HEADER_HEIGHT))
        .flex_none()
        .relative()
        .bg(rgb(HEADER_BG))
        .border_r_1()
        .border_b_1()
        .border_color(rgb(GRID_COLOR))
        .on_mouse_down(MouseButton::Left, move |_, _, cx| {
            entity.update(cx, |viewer, cx| {
                if viewer.select_sheet() {
                    cx.notify();
                }
            });
        })
        .when(layout.pinned_columns == 0, |element| {
            element.child(
                div()
                    .absolute()
                    .right(px(0.0))
                    .top(px(0.0))
                    .w(px(FREEZE_HANDLE_HIT_SIZE))
                    .h_full()
                    .cursor(CursorStyle::ResizeColumn)
                    .child(
                        div()
                            .absolute()
                            .right(px(0.0))
                            .top(px(0.0))
                            .w(px(FREEZE_HANDLE_VISUAL_SIZE))
                            .h_full()
                            .bg(rgb(FREEZE_LINE_COLOR)),
                    )
                    .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                        column_freeze_entity.update(cx, |viewer, cx| {
                            viewer.start_column_freeze_drag_from_zero(event.position.x);
                            cx.notify();
                        });
                        cx.stop_propagation();
                    }),
            )
        })
        .when(layout.pinned_rows == 0, |element| {
            element.child(
                div()
                    .absolute()
                    .left(px(0.0))
                    .bottom(px(0.0))
                    .w_full()
                    .h(px(FREEZE_HANDLE_HIT_SIZE))
                    .cursor(CursorStyle::ResizeRow)
                    .child(
                        div()
                            .absolute()
                            .left(px(0.0))
                            .bottom(px(0.0))
                            .w_full()
                            .h(px(FREEZE_HANDLE_VISUAL_SIZE))
                            .bg(rgb(FREEZE_LINE_COLOR)),
                    )
                    .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                        row_freeze_entity.update(cx, |viewer, cx| {
                            viewer.start_row_freeze_drag_from_zero(event.position.y);
                            cx.notify();
                        });
                        cx.stop_propagation();
                    }),
            )
        })
}

fn column_headers_range(
    workbook: &WorkbookData,
    sheet_ix: usize,
    layout: &SheetLayout,
    selection: Selection,
    entity: &Entity<SpreadsheetViewer>,
    start_col: usize,
    end_col: usize,
) -> Div {
    let sheet = workbook.sheet(sheet_ix);
    let end_col = end_col.min(sheet.col_count());
    let mut row = div()
        .flex()
        .h(px(HEADER_HEIGHT))
        .w(px(layout.columns_width(start_col, end_col)));

    for col_ix in start_col..end_col {
        row = row.child(column_header(
            column_name(col_ix),
            layout.column_width(col_ix),
            col_ix,
            selection.range.intersects_col(col_ix),
            entity,
        ));
    }

    row
}

fn column_header(
    label: String,
    width: f32,
    col_ix: usize,
    selected: bool,
    entity: &Entity<SpreadsheetViewer>,
) -> Div {
    let entity: Entity<SpreadsheetViewer> = (*entity).clone();
    let resize_entity = entity.clone();
    let drag_entity = entity.clone();

    div()
        .w(px(width))
        .h(px(HEADER_HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .overflow_hidden()
        .whitespace_nowrap()
        .relative()
        .bg(rgb(if selected {
            selection_tint(HEADER_BG)
        } else {
            HEADER_BG
        }))
        .border_r_1()
        .border_b_1()
        .border_color(rgb(GRID_COLOR))
        .text_color(rgb(HEADER_TEXT))
        .child(label)
        .child(
            div()
                .absolute()
                .right(px(0.0))
                .top(px(0.0))
                .w(px(RESIZE_HANDLE_SIZE))
                .h_full()
                .cursor(CursorStyle::ResizeColumn)
                .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                    resize_entity.update(cx, |viewer, cx| {
                        viewer.start_column_resize(col_ix, event.position.x);
                        cx.notify();
                    });
                    cx.stop_propagation();
                })
                .on_mouse_move(move |event, _, cx| {
                    if !event.dragging() {
                        return;
                    }

                    drag_entity.update(cx, |viewer, cx| {
                        if viewer.drag_resize(event.position) {
                            cx.notify();
                        }
                    });
                    cx.stop_propagation();
                }),
        )
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            entity.update(cx, |viewer, cx| {
                if viewer.select_col(col_ix, event.modifiers.shift) {
                    cx.notify();
                }
            });
        })
}

fn vertical_freeze_line(entity: Entity<SpreadsheetViewer>) -> Div {
    div()
        .absolute()
        .right(px(-(FREEZE_LINE_SIZE / 2.0)))
        .top(px(0.0))
        .w(px(FREEZE_LINE_SIZE))
        .h_full()
        .bg(rgb(FREEZE_LINE_COLOR))
        .cursor(CursorStyle::ResizeColumn)
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            entity.update(cx, |viewer, cx| {
                viewer.start_column_freeze_drag(event.position.x);
                cx.notify();
            });
            cx.stop_propagation();
        })
}

fn horizontal_freeze_line(entity: Entity<SpreadsheetViewer>) -> Div {
    div()
        .absolute()
        .left(px(0.0))
        .bottom(px(-(FREEZE_LINE_SIZE / 2.0)))
        .w_full()
        .h(px(FREEZE_LINE_SIZE))
        .bg(rgb(FREEZE_LINE_COLOR))
        .cursor(CursorStyle::ResizeRow)
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            entity.update(cx, |viewer, cx| {
                viewer.start_row_freeze_drag(event.position.y);
                cx.notify();
            });
            cx.stop_propagation();
        })
}

fn row_header(
    row_ix: usize,
    row_height: f32,
    width: f32,
    selected: bool,
    entity: &Entity<SpreadsheetViewer>,
) -> Div {
    let entity: Entity<SpreadsheetViewer> = (*entity).clone();
    let resize_entity = entity.clone();
    let drag_entity = entity.clone();

    div()
        .w(px(width))
        .h(px(row_height))
        .flex()
        .items_center()
        .justify_end()
        .flex_none()
        .px_2()
        .overflow_hidden()
        .whitespace_nowrap()
        .relative()
        .bg(rgb(if selected {
            selection_tint(HEADER_BG)
        } else {
            HEADER_BG
        }))
        .border_r_1()
        .border_b_1()
        .border_color(rgb(GRID_COLOR))
        .text_color(rgb(HEADER_TEXT))
        .child(row_number_label(row_ix + 1))
        .child(
            div()
                .absolute()
                .left(px(0.0))
                .bottom(px(0.0))
                .w_full()
                .h(px(RESIZE_HANDLE_SIZE))
                .cursor(CursorStyle::ResizeRow)
                .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                    resize_entity.update(cx, |viewer, cx| {
                        viewer.start_row_resize(row_ix, event.position.y);
                        cx.notify();
                    });
                    cx.stop_propagation();
                })
                .on_mouse_move(move |event, _, cx| {
                    if !event.dragging() {
                        return;
                    }

                    drag_entity.update(cx, |viewer, cx| {
                        if viewer.drag_resize(event.position) {
                            cx.notify();
                        }
                    });
                    cx.stop_propagation();
                }),
        )
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            entity.update(cx, |viewer, cx| {
                if viewer.select_row(row_ix, event.modifiers.shift) {
                    cx.notify();
                }
            });
        })
}

fn cell(
    row_cell: &RowCell,
    width: f32,
    row_height: f32,
    state: CellRenderState,
    render_text: bool,
    entity: &Entity<SpreadsheetViewer>,
) -> Div {
    let entity: Entity<SpreadsheetViewer> = (*entity).clone();
    let highlighted = state.active || state.selected;
    let cell = &row_cell.data;
    let cell_text = row_cell.text.as_str();
    let formula_fallback = row_cell.formula_fallback;
    let multiline = cell.style.wrap_text || cell_text.contains('\n');
    let text_color = if formula_fallback {
        FORMULA_FALLBACK_TEXT
    } else {
        cell.style.text_color.unwrap_or(CELL_TEXT)
    };
    let border_color = if state.selected {
        SELECTION_INNER_BORDER
    } else {
        GRID_COLOR
    };
    // Merged cells (anchor or covered) are painted by the merge overlay layer;
    // the base cell stays blank and borderless so the region looks unified.
    let merged = row_cell.merge != MergeKind::None;
    let mut element = div()
        .w(px(width))
        .h(px(row_height))
        .flex()
        .flex_none()
        .px_2()
        .overflow_hidden()
        .relative()
        .when(!merged, |element| {
            element
                .border_1()
                .border_color(rgb(border_color))
                .border_l_0()
                .border_t_0()
        })
        .bg(rgb({
            let base = cell.style.background_color.unwrap_or(CELL_BG);
            if state.selected && !state.active {
                selection_tint(base)
            } else {
                base
            }
        }))
        .text_color(rgb(text_color));

    element = if multiline {
        element.items_start().whitespace_normal()
    } else {
        element.items_center().whitespace_nowrap()
    };

    if cell.style.bold {
        element = element.font_weight(FontWeight::BOLD);
    }

    let drag_entity = entity.clone();
    let mut element = if render_text && !merged {
        element.child(cell_text.to_owned())
    } else {
        element
    };

    if highlighted && !merged {
        element = element.child(selection_outline(state.selection_edges));
    }

    element
        .on_mouse_down(MouseButton::Left, move |event, _, cx| {
            entity.update(cx, |viewer, cx| {
                if viewer.select_cell(state.coord, event.modifiers.shift) {
                    cx.notify();
                }
            });
        })
        .on_mouse_move(move |event, _, cx| {
            if !event.dragging() {
                return;
            }

            drag_entity.update(cx, |viewer, cx| {
                if viewer.drag_to_cell(state.coord) {
                    cx.notify();
                }
            });
        })
}

fn cell_text_overlay(
    row_cell: &RowCell,
    left: f32,
    source_width: f32,
    width: f32,
    row_height: f32,
    state: CellRenderState,
) -> Div {
    let cell = &row_cell.data;
    let mask_width = (row_cell.text_width + CELL_HORIZONTAL_PADDING).min(width);
    let source_mask_width = mask_width.min(source_width);
    let overflow_mask_width = (mask_width - source_width).max(0.0);
    let source_background = {
        let base = cell.style.background_color.unwrap_or(CELL_BG);
        if state.selected && !state.active {
            selection_tint(base)
        } else {
            base
        }
    };
    let text_color = if row_cell.formula_fallback {
        FORMULA_FALLBACK_TEXT
    } else {
        cell.style.text_color.unwrap_or(CELL_TEXT)
    };
    let multiline = row_cell.text.contains('\n');
    let mut text_layer = div()
        .absolute()
        .left(px(0.0))
        .top(px(0.0))
        .w(px(mask_width))
        .h(px((row_height - 1.0).max(0.0)))
        .flex()
        .px_2()
        .overflow_hidden()
        .text_color(rgb(text_color))
        .when(!multiline, |element| {
            element.items_center().whitespace_nowrap()
        })
        .when(multiline, |element| {
            element.items_start().whitespace_normal()
        });

    if cell.style.bold {
        text_layer = text_layer.font_weight(FontWeight::BOLD);
    }

    let highlighted = state.active || state.selected;
    let mut overlay = div()
        .absolute()
        .left(px(left))
        .top(px(0.0))
        .w(px(width))
        .h(px(row_height))
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .left(px(0.0))
                .top(px(0.0))
                .w(px(source_mask_width))
                .h(px((row_height - 1.0).max(0.0)))
                .bg(rgb(source_background)),
        )
        .when(overflow_mask_width > 0.0, |element| {
            element.child(
                div()
                    .absolute()
                    .left(px(source_width))
                    .top(px(0.0))
                    .w(px(overflow_mask_width))
                    .h(px((row_height - 1.0).max(0.0)))
                    .bg(rgb(CELL_BG)),
            )
        })
        .child(text_layer.child(row_cell.text.clone()));

    if highlighted {
        overlay = overlay.child(
            div()
                .absolute()
                .left(px(0.0))
                .top(px(0.0))
                .w(px(source_width))
                .h(px(row_height))
                .child(selection_outline(state.selection_edges)),
        );
    }

    overlay
}

fn cell_display_text(cell: &CellData) -> (String, bool) {
    let Some(formula) = cell
        .formula
        .as_deref()
        .filter(|formula| !formula.is_empty())
    else {
        return (cell.value.clone(), false);
    };

    if cell.value.is_empty() {
        (formula_display_text(formula), true)
    } else {
        (cell.value.clone(), false)
    }
}

fn formula_display_text(formula: &str) -> String {
    if formula.starts_with('=') {
        formula.to_owned()
    } else {
        format!("={formula}")
    }
}

#[cfg(target_os = "macos")]
fn write_rich_clipboard(text: &str, html: &str, _: &mut Context<'_, SpreadsheetViewer>) {
    use cocoa::{
        appkit::{NSPasteboard, NSPasteboardTypeString},
        base::nil,
        foundation::NSString,
    };

    // Cocoa pasteboard calls are confined to the UI thread by GPUI action dispatch.
    unsafe {
        let pasteboard = NSPasteboard::generalPasteboard(nil);
        pasteboard.clearContents();

        let plain_text = NSString::alloc(nil).init_str(text);
        pasteboard.setString_forType(plain_text, NSPasteboardTypeString);

        let html_string = NSString::alloc(nil).init_str(html);
        let public_html_type = NSString::alloc(nil).init_str("public.html");
        pasteboard.setString_forType(html_string, public_html_type);

        let text_html_type = NSString::alloc(nil).init_str("text/html");
        pasteboard.setString_forType(html_string, text_html_type);
    }
}

#[cfg(not(target_os = "macos"))]
fn write_rich_clipboard(text: &str, _: &str, cx: &mut Context<'_, SpreadsheetViewer>) {
    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.to_owned()));
}

fn selection_outline(edges: SelectionEdgeSides) -> AnyElement {
    if !edges.any() {
        return div().into_any_element();
    }

    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let color = rgb(SELECTION_BORDER);
            let thickness = px(1.0);

            if edges.top() {
                window.paint_quad(gpui::fill(
                    Bounds {
                        origin: bounds.origin,
                        size: gpui::size(bounds.size.width, thickness),
                    },
                    color,
                ));
            }
            if edges.bottom() {
                window.paint_quad(gpui::fill(
                    Bounds {
                        origin: point(
                            bounds.origin.x,
                            bounds.origin.y + bounds.size.height - thickness,
                        ),
                        size: gpui::size(bounds.size.width, thickness),
                    },
                    color,
                ));
            }
            if edges.left() {
                window.paint_quad(gpui::fill(
                    Bounds {
                        origin: bounds.origin,
                        size: gpui::size(thickness, bounds.size.height),
                    },
                    color,
                ));
            }
            if edges.right() {
                window.paint_quad(gpui::fill(
                    Bounds {
                        origin: point(
                            bounds.origin.x + bounds.size.width - thickness,
                            bounds.origin.y,
                        ),
                        size: gpui::size(thickness, bounds.size.height),
                    },
                    color,
                ));
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
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

    #[test]
    fn column_names_match_spreadsheet_labels() {
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(27), "AB");
        assert_eq!(column_name(701), "ZZ");
        assert_eq!(column_name(702), "AAA");
    }

    #[test]
    fn column_index_inverts_column_name() {
        assert_eq!(column_index("A"), Some(0));
        assert_eq!(column_index("Z"), Some(25));
        assert_eq!(column_index("AA"), Some(26));
        assert_eq!(column_index("AB"), Some(27));
        assert_eq!(column_index("ZZ"), Some(701));
        assert_eq!(column_index("AAA"), Some(702));
    }

    #[test]
    fn parses_column_row_and_cell_references() {
        assert_eq!(
            parse_name_box_reference("A", 10, 1000),
            Some(NameRef::Column(0))
        );
        assert_eq!(
            parse_name_box_reference("100", 10, 1000),
            Some(NameRef::Row(99))
        );
        assert_eq!(
            parse_name_box_reference(" b101 ", 10, 1000),
            Some(NameRef::Cell(CellCoord::new(100, 1)))
        );
    }

    #[test]
    fn name_box_references_clamp_and_reject_bad_input() {
        assert_eq!(
            parse_name_box_reference("ZZ", 3, 5),
            Some(NameRef::Column(2))
        );
        assert_eq!(
            parse_name_box_reference("9999", 3, 5),
            Some(NameRef::Row(4))
        );
        assert_eq!(parse_name_box_reference("", 3, 5), None);
        assert_eq!(parse_name_box_reference("1A", 3, 5), None);
        assert_eq!(parse_name_box_reference("A0", 3, 5), None);
        assert_eq!(parse_name_box_reference("A1!", 3, 5), None);
        assert_eq!(parse_name_box_reference("A1", 0, 0), None);
    }

    #[test]
    fn blank_cached_formula_displays_formula_fallback() {
        let formula_cell = CellData {
            value: String::new(),
            formula: Some("'Engineering'!B7".to_owned()),
            ..Default::default()
        };
        let cached_formula_cell = CellData {
            value: "18".to_owned(),
            formula: Some("'Engineering'!B7".to_owned()),
            ..Default::default()
        };

        assert_eq!(
            cell_display_text(&formula_cell),
            ("='Engineering'!B7".to_owned(), true)
        );
        assert_eq!(
            cell_display_text(&cached_formula_cell),
            ("18".to_owned(), false)
        );
    }

    #[test]
    fn formula_bar_expands_for_multiline_values() {
        assert_float_eq(formula_bar_height("one line"), FORMULA_BAR_HEIGHT);
        assert!(formula_bar_height("line 1\nline 2\nline 3") > FORMULA_BAR_HEIGHT);
    }

    #[test]
    fn sheet_layout_resizes_columns_and_rows_with_minimums() {
        let mut layout = SheetLayout {
            column_widths: vec![100.0, 120.0],
            rows: RowLayout::from_explicit_heights(vec![24.0, 30.0]),
            sheet_width: 220.0,
            row_header_width: MIN_ROW_HEADER_WIDTH,
            pinned_rows: 0,
            pinned_columns: 0,
            merges: SheetMerges::default(),
        };

        layout.set_column_widths(&[(0, 140.0), (1, 1.0)]);
        layout.set_row_heights(&[(0, 40.0), (1, 1.0)]);

        assert_float_eq(layout.column_width(0), 140.0);
        assert_float_eq(layout.column_width(1), MIN_COLUMN_WIDTH);
        assert_float_eq(layout.sheet_width, 140.0 + MIN_COLUMN_WIDTH);
        assert_float_eq(layout.row_height(0), 40.0);
        assert_float_eq(layout.row_height(1), MIN_ROW_HEIGHT);
        assert_float_eq(layout.sheet_height(), 40.0 + MIN_ROW_HEIGHT);
        let (row_ix, offset) = layout.row_offset_for_scroll_position(41.0);
        assert_eq!(row_ix, 1);
        assert_float_eq(offset, 1.0);
        assert_float_eq(layout.scroll_position_for_row_offset(1, 3.0), 43.0);
    }

    #[test]
    fn sheet_layout_clamps_pinned_rows_and_columns_to_bounds() {
        let mut layout = layout_with_columns(vec![50.0, 60.0]);
        layout.rows = RowLayout::from_explicit_heights(vec![20.0, 30.0, 40.0]);

        assert!(layout.set_pinned_columns(99));
        assert!(layout.set_pinned_rows(99));

        assert_eq!(layout.pinned_columns, 2);
        assert_eq!(layout.pinned_rows, 3);
        assert_float_eq(layout.pinned_column_width(), 110.0);
        assert_float_eq(layout.pinned_row_height(), 90.0);
        assert_float_eq(layout.scrollable_width(), 0.0);
        assert_float_eq(layout.scrollable_height(), 0.0);
    }

    #[test]
    fn visible_scrollable_column_range_uses_viewport_with_overscan() {
        let mut layout = layout_with_columns(vec![50.0, 60.0, 70.0, 80.0, 90.0, 100.0]);
        layout.set_pinned_columns(1);

        assert_eq!(layout.visible_scrollable_column_range(0.0, 130.0), 1..5);
        assert_eq!(layout.visible_scrollable_column_range(260.0, 60.0), 2..6);
        assert_eq!(layout.visible_scrollable_column_range(0.0, 0.0), 1..1);
    }

    #[test]
    fn freeze_targets_snap_to_nearest_column_and_row_boundary() {
        let mut layout = layout_with_columns(vec![50.0, 100.0, 80.0]);
        layout.rows = RowLayout::from_explicit_heights(vec![20.0, 30.0, 40.0]);

        assert_eq!(freeze_target_column(&layout, -1.0), 0);
        assert_eq!(freeze_target_column(&layout, 24.0), 0);
        assert_eq!(freeze_target_column(&layout, 26.0), 1);
        assert_eq!(freeze_target_column(&layout, 101.0), 2);
        assert_eq!(freeze_target_column(&layout, 999.0), 3);

        assert_eq!(freeze_target_row(&layout, 9.0), 0);
        assert_eq!(freeze_target_row(&layout, 11.0), 1);
        assert_eq!(freeze_target_row(&layout, 36.0), 2);
        assert_eq!(freeze_target_row(&layout, 999.0), 3);
    }

    #[test]
    fn scrollable_row_top_ignores_pinned_rows() {
        let mut layout = layout_with_columns(vec![100.0]);
        layout.rows = RowLayout::from_explicit_heights(vec![20.0, 30.0, 40.0, 50.0]);
        layout.set_pinned_rows(2);

        // Pinned rows 0,1 occupy 50px; the scrollable region starts at row 2.
        assert_float_eq(layout.scrollable_row_top(2), 0.0);
        assert_float_eq(layout.scrollable_row_top(3), 40.0);
        assert_float_eq(layout.scrollable_height(), 90.0);
    }

    #[test]
    fn visible_scrollable_row_range_is_constant_for_huge_uniform_sheets() {
        let mut layout = layout_with_columns(vec![100.0]);
        layout.rows = RowLayout::new(SheetRowLayout::Uniform {
            row_count: 37_000_000,
            height: 20.0,
        });
        layout.set_pinned_rows(1);

        // Scrolled far down: the range stays small (windowed) and starts near
        // the row at the scroll position, never enumerating all rows.
        let range = layout.visible_scrollable_row_range(1_000_000.0, 200.0);
        assert!(range.start >= layout.pinned_rows);
        assert!(
            range.len() <= 20 + 2 * ROW_RENDER_OVERSCAN,
            "windowed range too large: {}",
            range.len()
        );
        // Row at absolute position 1_000_000 + pinned(20px) = 1_000_020 -> row 50001.
        assert!(range.contains(&50_001));

        assert_eq!(
            layout.visible_scrollable_row_range(0.0, 0.0),
            layout.pinned_rows..layout.pinned_rows
        );
    }

    #[test]
    fn scrollbar_visibility_uses_only_scrollable_panes() {
        let mut layout = layout_with_columns(vec![60.0, 60.0, 60.0]);
        layout.rows = RowLayout::from_explicit_heights(vec![30.0, 30.0, 30.0]);
        layout.sheet_width = 180.0;
        layout.set_pinned_columns(1);
        layout.set_pinned_rows(1);

        let visibility = scrollbar_visibility_for_window_size(
            &layout,
            FORMULA_BAR_HEIGHT,
            layout.row_header_width + 130.0,
            fixed_view_height() + 70.0,
        );

        assert!(visibility.horizontal);
        assert!(visibility.vertical);
    }

    #[test]
    fn uniform_row_layout_maps_scroll_positions_without_offsets() {
        let mut layout = SheetLayout {
            column_widths: vec![100.0],
            rows: RowLayout::new(SheetRowLayout::Uniform {
                row_count: 1_000_000,
                height: 24.0,
            }),
            sheet_width: 100.0,
            row_header_width: row_header_width(1_000_000),
            pinned_rows: 0,
            pinned_columns: 0,
            merges: SheetMerges::default(),
        };

        assert_float_eq(layout.row_height(999_999), 24.0);
        assert_float_eq(layout.sheet_height(), 24_000_000.0);
        assert_eq!(layout.row_offset_for_scroll_position(48.5), (2, 0.5));
        assert_float_eq(layout.scroll_position_for_row_offset(3, 4.0), 76.0);

        layout.set_row_heights(&[(0, 30.0), (1, 30.0), (2, 30.0)]);

        assert_float_eq(layout.row_height(0), 30.0);
        assert_float_eq(layout.row_height(3), 24.0);
    }

    #[test]
    fn row_header_width_grows_for_large_row_counts() {
        assert_float_eq(row_header_width(0), MIN_ROW_HEADER_WIDTH);
        assert_float_eq(row_header_width(999), MIN_ROW_HEADER_WIDTH);
        assert!(row_header_width(9_999) > MIN_ROW_HEADER_WIDTH);
        assert!(row_header_width(1_000_000) > row_header_width(100_000));
    }

    #[test]
    fn row_number_labels_use_thousand_separators() {
        assert_eq!(row_number_label(1), "1");
        assert_eq!(row_number_label(999), "999");
        assert_eq!(row_number_label(1_000), "1,000");
        assert_eq!(row_number_label(240_041), "240,041");
        assert_eq!(row_number_label(1_000_000), "1,000,000");
    }

    #[test]
    fn scrollbar_visibility_hides_bars_when_sheet_fits() {
        let layout = test_layout(100.0, 80.0);
        let visibility = scrollbar_visibility_for_window_size(
            &layout,
            FORMULA_BAR_HEIGHT,
            layout.row_header_width + 120.0,
            fixed_view_height() + 100.0,
        );

        assert!(!visibility.horizontal);
        assert!(!visibility.vertical);
    }

    #[test]
    fn vertical_scrollbar_can_require_horizontal_scrollbar() {
        let layout = test_layout(105.0, 200.0);
        let visibility = scrollbar_visibility_for_window_size(
            &layout,
            FORMULA_BAR_HEIGHT,
            layout.row_header_width + 110.0,
            fixed_view_height() + 100.0,
        );

        assert!(visibility.vertical);
        assert!(visibility.horizontal);
    }

    #[test]
    fn horizontal_scrollbar_can_require_vertical_scrollbar() {
        let layout = test_layout(200.0, 95.0);
        let visibility = scrollbar_visibility_for_window_size(
            &layout,
            FORMULA_BAR_HEIGHT,
            layout.row_header_width + 100.0,
            fixed_view_height() + 100.0,
        );

        assert!(visibility.horizontal);
        assert!(visibility.vertical);
    }

    #[test]
    fn text_overflow_extends_across_empty_cells_until_next_value() {
        let layout = layout_with_columns(vec![50.0, 60.0, 70.0, 80.0]);
        let cells = vec![
            text_row_cell("Long account name"),
            empty_row_cell(),
            text_row_cell("Next value"),
            empty_row_cell(),
        ];

        let widths = overflow_text_widths(&cells, &layout);

        assert_float_eq(widths[0].expect("first text cell should overflow"), 110.0);
        assert!(widths[1].is_none());
        assert_float_eq(widths[2].expect("third text cell should overflow"), 150.0);
        assert!(widths[3].is_none());
    }

    #[test]
    fn text_overflow_stays_clipped_when_right_cell_has_value() {
        let layout = layout_with_columns(vec![50.0, 60.0]);
        let cells = vec![text_row_cell("Long account name"), text_row_cell("Value")];

        let widths = overflow_text_widths(&cells, &layout);

        assert!(widths.iter().all(Option::is_none));
    }

    #[test]
    fn fitting_text_does_not_mask_empty_cell_grid_lines() {
        let layout = layout_with_columns(vec![120.0, 60.0]);
        let cells = vec![text_row_cell("Short"), empty_row_cell()];

        let widths = overflow_text_widths(&cells, &layout);

        assert!(widths.iter().all(Option::is_none));
    }

    #[test]
    fn multiline_text_overflow_uses_widest_line() {
        let layout = layout_with_columns(vec![50.0, 60.0]);
        let cells = vec![text_row_cell("short\nMuch longer line"), empty_row_cell()];

        let widths = overflow_text_widths(&cells, &layout);

        assert_float_eq(widths[0].expect("multiline text should overflow"), 110.0);
        assert!(widths[1].is_none());
    }

    #[test]
    fn non_text_cells_do_not_overflow_into_empty_cells() {
        let layout = layout_with_columns(vec![50.0, 60.0]);
        let cells = vec![
            RowCell {
                data: CellData {
                    value: "1234567890".to_owned(),
                    raw_value: CellRawValue::Number(1_234_567_890.0),
                    ..Default::default()
                },
                text: "1234567890".to_owned(),
                text_width: 100.0,
                formula_fallback: false,
                merge: MergeKind::None,
            },
            empty_row_cell(),
        ];

        let widths = overflow_text_widths(&cells, &layout);

        assert!(widths.iter().all(Option::is_none));
    }

    #[test]
    fn text_does_not_overflow_into_a_merged_cell() {
        let layout = layout_with_columns(vec![50.0, 60.0, 60.0]);
        let cells = vec![
            text_row_cell("a long piece of text that wants room"),
            merged_row_cell(MergeKind::Covered),
            empty_row_cell(),
        ];

        let widths = overflow_text_widths(&cells, &layout);

        assert!(widths[0].is_none());
    }

    #[test]
    fn a_merge_anchor_does_not_text_overflow() {
        let layout = layout_with_columns(vec![50.0, 60.0]);
        let mut anchor = text_row_cell("a long piece of text that wants room");
        anchor.merge = MergeKind::Anchor;
        let cells = vec![anchor, empty_row_cell()];

        let widths = overflow_text_widths(&cells, &layout);

        assert!(widths[0].is_none());
    }

    #[test]
    fn selection_tint_blends_over_the_real_background() {
        // Tinting nudges a color toward the selection blue without replacing it,
        // so distinct backgrounds stay distinct under the highlight.
        let white = selection_tint(CELL_BG);
        let amber = selection_tint(0xff_e9_b0);
        assert_ne!(white, CELL_BG);
        assert_ne!(white, amber);
        // White moves toward blue: the blue channel stays highest and brightness drops.
        assert!(white & 0xff > (white >> 16) & 0xff);
        assert!(white < CELL_BG);
    }

    #[test]
    fn lazy_sheets_update_less_often_during_scrollbar_drag() {
        assert_eq!(
            vertical_scroll_drag_update_interval(true),
            VERTICAL_SCROLL_DRAG_UPDATE_INTERVAL
        );
        assert_eq!(
            vertical_scroll_drag_update_interval(false),
            LAZY_VERTICAL_SCROLL_DRAG_UPDATE_INTERVAL
        );
    }

    fn text_row_cell(text: &str) -> RowCell {
        RowCell {
            data: CellData {
                value: text.to_owned(),
                raw_value: CellRawValue::Text,
                ..Default::default()
            },
            text: text.to_owned(),
            text_width: text.chars().count() as f32 * 7.0,
            formula_fallback: false,
            merge: MergeKind::None,
        }
    }

    fn empty_row_cell() -> RowCell {
        RowCell {
            data: CellData::default(),
            text: String::new(),
            text_width: 0.0,
            formula_fallback: false,
            merge: MergeKind::None,
        }
    }

    fn merged_row_cell(merge: MergeKind) -> RowCell {
        RowCell {
            merge,
            ..empty_row_cell()
        }
    }

    fn test_layout(sheet_width: f32, sheet_height: f32) -> SheetLayout {
        SheetLayout {
            column_widths: vec![sheet_width],
            rows: RowLayout::from_explicit_heights(vec![sheet_height]),
            sheet_width,
            row_header_width: MIN_ROW_HEADER_WIDTH,
            pinned_rows: 0,
            pinned_columns: 0,
            merges: SheetMerges::default(),
        }
    }

    fn layout_with_columns(column_widths: Vec<f32>) -> SheetLayout {
        SheetLayout {
            sheet_width: column_widths.iter().sum(),
            column_widths,
            rows: RowLayout::from_explicit_heights(vec![crate::workbook::DEFAULT_ROW_HEIGHT]),
            row_header_width: MIN_ROW_HEADER_WIDTH,
            pinned_rows: 0,
            pinned_columns: 0,
            merges: SheetMerges::default(),
        }
    }

    fn fixed_view_height() -> f32 {
        TITLE_BAR_HEIGHT + FORMULA_BAR_HEIGHT + HEADER_HEIGHT + FOOTER_HEIGHT
    }

    fn assert_float_eq(left: f32, right: f32) {
        assert!((left - right).abs() < f32::EPSILON, "{left} != {right}");
    }
}
