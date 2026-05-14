use std::{
    cell::Cell,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{
    AnyElement, App, Bounds, Context, CursorStyle, Div, Entity, FocusHandle, Focusable, FontWeight,
    IntoElement, ListAlignment, ListOffset, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Render, ScrollHandle, Stateful, TextRun, Window, actions, canvas,
    div, font, list, point, prelude::*, px, rgb,
};

use crate::{
    CloseFile,
    workbook::{
        CellCoord, CellData, CellRange, CellRawValue, SelectionEdgeSides, SheetData,
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
const ACTIVE_CELL_BG: u32 = 0xd2_e3_fc;
const SELECTION_BORDER: u32 = 0x1a_73_e8;
const SELECTION_INNER_BORDER: u32 = 0xa8_c7_fa;
const TITLE_BAR_HEIGHT: f32 = 40.0;
const FORMULA_BAR_HEIGHT: f32 = 36.0;
const FORMULA_BAR_LINE_HEIGHT: f32 = 17.0;
const FORMULA_BAR_VERTICAL_PADDING: f32 = 12.0;
const CELL_HORIZONTAL_PADDING: f32 = 16.0;
const FOOTER_HEIGHT: f32 = 32.0;
const RESIZE_HANDLE_SIZE: f32 = 6.0;
const MIN_COLUMN_WIDTH: f32 = 24.0;
const MIN_ROW_HEIGHT: f32 = 18.0;
const VERTICAL_SCROLL_DRAG_UPDATE_INTERVAL: Duration = Duration::from_millis(50);
const LAZY_VERTICAL_SCROLL_DRAG_UPDATE_INTERVAL: Duration = Duration::from_millis(200);

pub(crate) const WINDOW_WIDTH: f32 = 1100.0;
pub(crate) const WINDOW_HEIGHT: f32 = 720.0;

actions!(spreadsheet_viewer, [CopySelection]);

pub(crate) struct SpreadsheetViewer {
    workbook: Arc<WorkbookData>,
    show_splash_after_close: Rc<Cell<bool>>,
    focus_handle: FocusHandle,
    active_sheet: usize,
    selection: Selection,
    selection_drag: Option<CellCoord>,
    summary_metric: SummaryMetric,
    show_summary_menu: bool,
    horizontal_scroll: ScrollHandle,
    tabs_scroll: ScrollHandle,
    body_list: ListState,
    scrollbar_drag: Option<ScrollbarDrag>,
    resize_drag: Option<ResizeDrag>,
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

struct RowCell {
    data: CellData,
    text: String,
    text_width: f32,
    formula_fallback: bool,
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
        }
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

impl SpreadsheetViewer {
    pub(crate) fn new(
        workbook: Arc<WorkbookData>,
        active_sheet: usize,
        show_splash_after_close: Rc<Cell<bool>>,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> Self {
        let body_list = ListState::new(
            workbook.sheet(active_sheet).row_count(),
            ListAlignment::Top,
            px(800.0),
        );
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);

        let layouts = (0..workbook.sheet_count())
            .map(|sheet_ix| SheetLayout::new(workbook.sheet(sheet_ix)))
            .collect();

        Self {
            workbook,
            show_splash_after_close,
            focus_handle,
            active_sheet,
            selection: Selection::single(CellCoord::new(0, 0)),
            selection_drag: None,
            summary_metric: SummaryMetric::Sum,
            show_summary_menu: false,
            horizontal_scroll: ScrollHandle::new(),
            tabs_scroll: ScrollHandle::new(),
            body_list,
            scrollbar_drag: None,
            resize_drag: None,
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
        self.selection_drag = None;
        self.show_summary_menu = false;
        self.horizontal_scroll.set_offset(point(px(0.0), px(0.0)));
        self.body_list.reset(self.active_sheet().row_count());
        true
    }

    fn select_cell(&mut self, coord: CellCoord, extend: bool) -> bool {
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
                self.invalidate_rows(updates.iter().map(|(row_ix, _)| *row_ix));
            }
        }

        true
    }

    fn invalidate_rows(&self, rows: impl IntoIterator<Item = usize>) {
        let mut rows = rows.into_iter().collect::<Vec<_>>();
        if rows.is_empty() {
            return;
        }

        rows.sort_unstable();
        rows.dedup();

        let mut range_start = rows[0];
        let mut previous = rows[0];
        for row_ix in rows.into_iter().skip(1) {
            if row_ix == previous + 1 {
                previous = row_ix;
                continue;
            }

            self.body_list
                .splice(range_start..previous + 1, previous + 1 - range_start);
            range_start = row_ix;
            previous = row_ix;
        }

        self.body_list
            .splice(range_start..previous + 1, previous + 1 - range_start);
    }

    fn end_resize(&mut self) {
        self.resize_drag = None;
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

    fn vertical_scroll_position(&self) -> Pixels {
        if let Some(ScrollbarDrag::Vertical {
            scroll_position, ..
        }) = self.scrollbar_drag
        {
            return scroll_position;
        }

        let scroll_top = self.body_list.logical_scroll_top();
        px(self.active_layout().scroll_position_for_row_offset(
            scroll_top.item_ix,
            f32::from(scroll_top.offset_in_item),
        ))
    }

    fn scroll_list_to_vertical_position(&self, list_state: &ListState, scroll_position: Pixels) {
        let (item_ix, offset_in_item) = self
            .active_layout()
            .row_offset_for_scroll_position(f32::from(scroll_position));
        list_state.scroll_to(ListOffset {
            item_ix,
            offset_in_item: px(offset_in_item),
        });
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
                });
            })
            .on_mouse_move({
                let entity = cx.entity();
                move |event, _, cx| {
                    if !event.dragging() {
                        return;
                    }

                    entity.update(cx, |viewer, cx| {
                        if viewer.drag_resize(event.position) {
                            cx.notify();
                        }
                    });
                }
            })
            .child(title_bar(workbook.as_ref(), sheet_ix))
            .child(formula_bar(workbook.sheet(sheet_ix), selection))
            .child(
                div()
                    .flex()
                    .h(px(HEADER_HEIGHT))
                    .flex_none()
                    .child(corner_header(layout.row_header_width, cx.entity()))
                    .child(column_header_pane(
                        &workbook,
                        sheet_ix,
                        &layout,
                        selection,
                        &self.horizontal_scroll,
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
                        layout.clone(),
                        selection,
                        &self.horizontal_scroll,
                        self.body_list.clone(),
                        &cx.entity(),
                    ))
                    .when(scrollbars.vertical, |element| {
                        element.child(vertical_scrollbar(self, cx))
                    }),
            )
            .when(scrollbars.horizontal, |element| {
                element.child(
                    div()
                        .flex()
                        .h(px(SCROLLBAR_SIZE))
                        .flex_none()
                        .child(div().w(px(layout.row_header_width)).h_full().flex_none())
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

    let mut visibility = ScrollbarVisibility {
        horizontal: layout.sheet_width > body_width,
        vertical: layout.sheet_height() > body_height,
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
            horizontal: layout.sheet_width > available_width,
            vertical: layout.sheet_height() > available_height,
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
    entity: &Entity<SpreadsheetViewer>,
) -> AnyElement {
    let workbook = Arc::clone(workbook);
    let horizontal_scroll = horizontal_scroll.clone();
    let scroll_entity: Entity<SpreadsheetViewer> = (*entity).clone();
    let header_entity: Entity<SpreadsheetViewer> = (*entity).clone();

    restrict_scroll_to_axis(
        div()
            .id("column-header-scroll")
            .flex_1()
            .h_full()
            .overflow_hidden()
            .track_scroll(&horizontal_scroll)
            .on_scroll_wheel(horizontal_scroll_handler(horizontal_scroll, scroll_entity))
            .child(column_headers(
                &workbook,
                sheet_ix,
                layout,
                selection,
                &header_entity,
            )),
    )
    .into_any_element()
}

fn body_pane(
    workbook: &Arc<WorkbookData>,
    sheet_ix: usize,
    layout: SheetLayout,
    selection: Selection,
    horizontal_scroll: &ScrollHandle,
    body_list: ListState,
    entity: &Entity<SpreadsheetViewer>,
) -> AnyElement {
    let workbook = Arc::clone(workbook);
    let horizontal_scroll = horizontal_scroll.clone();
    let row_horizontal_scroll = horizontal_scroll.clone();
    let scroll_entity: Entity<SpreadsheetViewer> = (*entity).clone();
    let list_entity: Entity<SpreadsheetViewer> = (*entity).clone();

    div()
        .id("sheet-body")
        .flex_1()
        .h_full()
        .overflow_hidden()
        .on_scroll_wheel(horizontal_scroll_handler(horizontal_scroll, scroll_entity))
        .child(
            list(body_list, move |row_ix, window, _| {
                let row_entity = list_entity.clone();
                render_body_row(
                    workbook.sheet(sheet_ix),
                    &layout,
                    row_ix,
                    row_horizontal_scroll.offset().x,
                    selection,
                    &row_entity,
                    window,
                )
            })
            .size_full(),
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

fn restrict_scroll_to_axis<E: Styled>(mut element: E) -> E {
    element.style().restrict_scroll_to_axis = Some(true);
    element
}

fn horizontal_scrollbar(
    viewer: &mut SpreadsheetViewer,
    cx: &mut Context<'_, SpreadsheetViewer>,
) -> AnyElement {
    let handle = viewer.horizontal_scroll.clone();
    let content_width = px(viewer.active_layout().sheet_width);

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
    cx: &mut Context<'_, SpreadsheetViewer>,
) -> AnyElement {
    let viewport_height = viewer.body_list.viewport_bounds().size.height;
    let content_height = px(viewer.active_layout().sheet_height()).max(viewport_height);
    let scroll_position = viewer.vertical_scroll_position();

    scrollbar_track("vertical-scrollbar-track")
        .w(px(SCROLLBAR_SIZE))
        .h_full()
        .flex_none()
        .child(list_scrollbar_thumb(
            scroll_position,
            content_height,
            viewer.body_list.clone(),
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

fn list_scrollbar_thumb(
    scroll_position: Pixels,
    content_size: Pixels,
    list_state: ListState,
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
                let list_state = list_state.clone();
                move |_: &MouseUpEvent, _, _, cx| {
                    entity.update(cx, |viewer, _| {
                        if let Some(ScrollbarDrag::Vertical {
                            scroll_position, ..
                        }) = viewer.scrollbar_drag
                        {
                            viewer.scroll_list_to_vertical_position(&list_state, scroll_position);
                        }
                        viewer.scrollbar_drag = None;
                    });
                    cx.notify(entity.entity_id());
                }
            });

            window.on_mouse_event({
                let entity = entity.clone();
                let list_state = list_state.clone();
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
                            viewer.scroll_list_to_vertical_position(&list_state, scroll_position);
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

fn formula_bar(sheet: &SheetData, selection: Selection) -> Div {
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
        .child(
            div()
                .w(px(72.0))
                .h(px(24.0))
                .mt(px(1.0))
                .flex()
                .items_center()
                .justify_center()
                .bg(rgb(CELL_BG))
                .border_1()
                .border_color(rgb(GRID_COLOR))
                .text_color(rgb(HEADER_TEXT))
                .child(cell_address(selection.anchor)),
        )
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

fn render_body_row(
    sheet: &SheetData,
    layout: &SheetLayout,
    row_ix: usize,
    horizontal_offset: Pixels,
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
        .child(
            div().flex_1().h(px(row_height)).overflow_hidden().child(
                render_cells_row(sheet, layout, row_ix, row_height, selection, entity, window)
                    .relative()
                    .left(horizontal_offset),
            ),
        )
        .into_any_element()
}

fn render_cells_row(
    sheet: &SheetData,
    layout: &SheetLayout,
    row_ix: usize,
    row_height: f32,
    selection: Selection,
    entity: &Entity<SpreadsheetViewer>,
    window: &mut Window,
) -> Stateful<Div> {
    let mut row = div()
        .id(("sheet-cells-row", row_ix))
        .flex()
        .relative()
        .h(px(row_height))
        .w(px(layout.sheet_width));

    let cells = (0..sheet.col_count())
        .map(|col_ix| {
            let data = sheet.cell_data(row_ix, col_ix);
            let (text, formula_fallback) = cell_display_text(&data);
            let text_width = measure_cell_text_width(&data, &text, formula_fallback, window);
            RowCell {
                data,
                text,
                text_width,
                formula_fallback,
            }
        })
        .collect::<Vec<_>>();
    let overflow_widths = overflow_text_widths(&cells, layout);

    for (col_ix, row_cell) in cells.iter().enumerate() {
        row = row.child(cell(
            row_cell,
            layout.column_width(col_ix),
            row_height,
            CellRenderState {
                coord: CellCoord::new(row_ix, col_ix),
                selected: selection.contains(row_ix, col_ix),
                selection_edges: selection.range.edge_sides(row_ix, col_ix),
                active: selection.anchor == CellCoord::new(row_ix, col_ix),
            },
            overflow_widths[col_ix].is_none(),
            entity,
        ));
    }

    let mut left = 0.0;
    for (col_ix, row_cell) in cells.iter().enumerate() {
        if let Some(width) = overflow_widths[col_ix] {
            row = row.child(cell_text_overlay(row_cell, left, width, row_height));
        }
        left += layout.column_width(col_ix);
    }

    row
}

fn overflow_text_widths(cells: &[RowCell], layout: &SheetLayout) -> Vec<Option<f32>> {
    let mut widths = vec![None; cells.len()];

    for col_ix in 0..cells.len() {
        let cell_content_width = (layout.column_width(col_ix) - CELL_HORIZONTAL_PADDING).max(0.0);
        let cell = &cells[col_ix];
        if !matches!(cell.data.raw_value, CellRawValue::Text)
            || cell.formula_fallback
            || cell.data.style.wrap_text
            || cell.text.is_empty()
            || cell.text_width <= cell_content_width
        {
            continue;
        }

        let mut end_col_ix = col_ix + 1;
        while end_col_ix < cells.len() && cells[end_col_ix].text.is_empty() {
            end_col_ix += 1;
        }

        if end_col_ix <= col_ix + 1 {
            continue;
        }

        widths[col_ix] = Some((col_ix..end_col_ix).map(|ix| layout.column_width(ix)).sum());
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

fn corner_header(width: f32, entity: Entity<SpreadsheetViewer>) -> Div {
    div()
        .w(px(width))
        .h(px(HEADER_HEIGHT))
        .flex_none()
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
}

fn column_headers(
    workbook: &WorkbookData,
    sheet_ix: usize,
    layout: &SheetLayout,
    selection: Selection,
    entity: &Entity<SpreadsheetViewer>,
) -> Div {
    let sheet = workbook.sheet(sheet_ix);
    let mut row = div().flex().h(px(HEADER_HEIGHT)).w(px(layout.sheet_width));

    for col_ix in 0..sheet.col_count() {
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
            SELECTED_CELL_BG
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
            SELECTED_CELL_BG
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
    let mut element = div()
        .w(px(width))
        .h(px(row_height))
        .flex()
        .flex_none()
        .px_2()
        .overflow_hidden()
        .relative()
        .border_1()
        .border_color(rgb(border_color))
        .bg(rgb(if state.active {
            ACTIVE_CELL_BG
        } else if state.selected {
            SELECTED_CELL_BG
        } else {
            cell.style.background_color.unwrap_or(CELL_BG)
        }))
        .text_color(rgb(text_color));

    element = if multiline {
        element.items_start().whitespace_normal()
    } else {
        element.items_center().whitespace_nowrap()
    };

    element = element.border_l_0().border_t_0();

    if cell.style.bold {
        element = element.font_weight(FontWeight::BOLD);
    }

    let drag_entity = entity.clone();
    let mut element = if render_text {
        element.child(cell_text.to_owned())
    } else {
        element
    };

    if highlighted {
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

fn cell_text_overlay(row_cell: &RowCell, left: f32, width: f32, row_height: f32) -> Div {
    let cell = &row_cell.data;
    let background_color = cell.style.background_color.unwrap_or(CELL_BG);
    let mask_width = (row_cell.text_width + CELL_HORIZONTAL_PADDING).min(width);
    let text_color = if row_cell.formula_fallback {
        FORMULA_FALLBACK_TEXT
    } else {
        cell.style.text_color.unwrap_or(CELL_TEXT)
    };
    let multiline = row_cell.text.contains('\n');
    let mut text = div()
        .w(px(mask_width))
        .h_full()
        .flex()
        .px_2()
        .overflow_hidden()
        .bg(rgb(background_color))
        .text_color(rgb(text_color))
        .when(!multiline, |element| {
            element.items_center().whitespace_nowrap()
        })
        .when(multiline, |element| {
            element.items_start().whitespace_normal()
        });

    if cell.style.bold {
        text = text.font_weight(FontWeight::BOLD);
    }

    div()
        .absolute()
        .left(px(left))
        .top(px(0.0))
        .w(px(width))
        .h(px((row_height - 1.0).max(0.0)))
        .overflow_hidden()
        .child(text.child(row_cell.text.clone()))
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
    fn uniform_row_layout_maps_scroll_positions_without_offsets() {
        let mut layout = SheetLayout {
            column_widths: vec![100.0],
            rows: RowLayout::new(SheetRowLayout::Uniform {
                row_count: 1_000_000,
                height: 24.0,
            }),
            sheet_width: 100.0,
            row_header_width: row_header_width(1_000_000),
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
            },
            empty_row_cell(),
        ];

        let widths = overflow_text_widths(&cells, &layout);

        assert!(widths.iter().all(Option::is_none));
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
        }
    }

    fn empty_row_cell() -> RowCell {
        RowCell {
            data: CellData::default(),
            text: String::new(),
            text_width: 0.0,
            formula_fallback: false,
        }
    }

    fn test_layout(sheet_width: f32, sheet_height: f32) -> SheetLayout {
        SheetLayout {
            column_widths: vec![sheet_width],
            rows: RowLayout::from_explicit_heights(vec![sheet_height]),
            sheet_width,
            row_header_width: MIN_ROW_HEADER_WIDTH,
        }
    }

    fn layout_with_columns(column_widths: Vec<f32>) -> SheetLayout {
        SheetLayout {
            sheet_width: column_widths.iter().sum(),
            column_widths,
            rows: RowLayout::from_explicit_heights(vec![crate::workbook::DEFAULT_ROW_HEIGHT]),
            row_header_width: MIN_ROW_HEADER_WIDTH,
        }
    }

    fn fixed_view_height() -> f32 {
        TITLE_BAR_HEIGHT + FORMULA_BAR_HEIGHT + HEADER_HEIGHT + FOOTER_HEIGHT
    }

    fn assert_float_eq(left: f32, right: f32) {
        assert!((left - right).abs() < f32::EPSILON, "{left} != {right}");
    }
}
