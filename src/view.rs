use std::sync::Arc;

use gpui::{
    AnyElement, App, Bounds, Context, Div, Entity, FontWeight, IntoElement, ListAlignment,
    ListState, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollHandle,
    Stateful, Window, canvas, div, list, point, prelude::*, px, rgb,
};

use crate::workbook::{CellData, WorkbookData};

const ROW_HEADER_WIDTH: f32 = 48.0;
const HEADER_HEIGHT: f32 = 24.0;
const SCROLLBAR_SIZE: f32 = 12.0;
const MIN_THUMB_SIZE: f32 = 32.0;
const GRID_COLOR: u32 = 0xd9_d9_d9;
const HEADER_BG: u32 = 0xf8_f9_fa;
const HEADER_TEXT: u32 = 0x3c_40_43;
const SHEET_BG: u32 = 0xfa_fa_fa;
const CELL_BG: u32 = 0xff_ff_ff;
const CELL_TEXT: u32 = 0x20_21_24;

pub(crate) const WINDOW_WIDTH: f32 = 1100.0;
pub(crate) const WINDOW_HEIGHT: f32 = 720.0;

pub(crate) struct SpreadsheetViewer {
    workbook: Arc<WorkbookData>,
    horizontal_scroll: ScrollHandle,
    body_list: ListState,
    scrollbar_drag: Option<ScrollbarDrag>,
}

#[derive(Clone, Copy)]
enum ScrollbarDrag {
    Horizontal { pointer_offset: Pixels },
    Vertical { pointer_offset: Pixels },
}

impl SpreadsheetViewer {
    pub(crate) fn new(workbook: Arc<WorkbookData>) -> Self {
        let body_list =
            ListState::new(workbook.row_count(), ListAlignment::Top, px(800.0)).measure_all();

        Self {
            workbook,
            horizontal_scroll: ScrollHandle::new(),
            body_list,
            scrollbar_drag: None,
        }
    }
}

impl Render for SpreadsheetViewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<'_, Self>) -> impl IntoElement {
        let workbook = Arc::clone(&self.workbook);

        div()
            .id("spreadsheet-viewport")
            .size_full()
            .bg(rgb(SHEET_BG))
            .text_color(rgb(CELL_TEXT))
            .text_size(px(12.0))
            .font_family("Arial")
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .h(px(HEADER_HEIGHT))
                    .flex_none()
                    .child(corner_header())
                    .child(column_header_pane(
                        &workbook,
                        &self.horizontal_scroll,
                        cx.entity(),
                    ))
                    .child(
                        div()
                            .w(px(SCROLLBAR_SIZE))
                            .h_full()
                            .flex_none()
                            .bg(rgb(HEADER_BG)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .child(body_pane(
                        &workbook,
                        &self.horizontal_scroll,
                        self.body_list.clone(),
                        cx.entity(),
                    ))
                    .child(vertical_scrollbar(self, cx)),
            )
            .child(
                div()
                    .flex()
                    .h(px(SCROLLBAR_SIZE))
                    .flex_none()
                    .child(div().w(px(ROW_HEADER_WIDTH)).h_full().flex_none())
                    .child(horizontal_scrollbar(self, cx))
                    .child(
                        div()
                            .w(px(SCROLLBAR_SIZE))
                            .h_full()
                            .flex_none()
                            .bg(rgb(HEADER_BG)),
                    ),
            )
    }
}

fn column_header_pane(
    workbook: &Arc<WorkbookData>,
    horizontal_scroll: &ScrollHandle,
    entity: Entity<SpreadsheetViewer>,
) -> AnyElement {
    let workbook = Arc::clone(workbook);
    let horizontal_scroll = horizontal_scroll.clone();

    restrict_scroll_to_axis(
        div()
            .id("column-header-scroll")
            .flex_1()
            .h_full()
            .overflow_hidden()
            .track_scroll(&horizontal_scroll)
            .on_scroll_wheel(horizontal_scroll_handler(horizontal_scroll, entity))
            .child(column_headers(&workbook)),
    )
    .into_any_element()
}

fn body_pane(
    workbook: &Arc<WorkbookData>,
    horizontal_scroll: &ScrollHandle,
    body_list: ListState,
    entity: Entity<SpreadsheetViewer>,
) -> AnyElement {
    let workbook = Arc::clone(workbook);
    let horizontal_scroll = horizontal_scroll.clone();
    let row_horizontal_scroll = horizontal_scroll.clone();

    div()
        .id("sheet-body")
        .flex_1()
        .h_full()
        .overflow_hidden()
        .on_scroll_wheel(horizontal_scroll_handler(horizontal_scroll, entity))
        .child(
            list(body_list, move |row_ix, _, _| {
                render_body_row(&workbook, row_ix, row_horizontal_scroll.offset().x)
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
        horizontal_scroll.set_offset(point(current.x + delta.x, current.y));
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
    let content_width = px(viewer.workbook.sheet_width());

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
    let content_height = viewport_height + viewer.body_list.max_offset_for_scrollbar().height;
    let offset = viewer.body_list.scroll_px_offset_for_scrollbar().y;

    scrollbar_track("vertical-scrollbar-track")
        .w(px(SCROLLBAR_SIZE))
        .h_full()
        .flex_none()
        .child(list_scrollbar_thumb(
            offset,
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
                            ScrollbarAxis::Vertical => ScrollbarDrag::Vertical { pointer_offset },
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
    scroll_offset: Pixels,
    content_size: Pixels,
    list_state: ListState,
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
                let list_state = list_state.clone();
                move |event: &MouseDownEvent, _, _, cx| {
                    if !thumb_bounds.contains(&event.position) {
                        return;
                    }

                    list_state.scrollbar_drag_started();
                    entity.update(cx, |viewer, _| {
                        viewer.scrollbar_drag = Some(ScrollbarDrag::Vertical {
                            pointer_offset: event.position.y - thumb_bounds.origin.y,
                        });
                    });
                }
            });

            window.on_mouse_event({
                let entity = entity.clone();
                let list_state = list_state.clone();
                move |_: &MouseUpEvent, _, _, cx| {
                    list_state.scrollbar_drag_ended();
                    entity.update(cx, |viewer, _| {
                        viewer.scrollbar_drag = None;
                    });
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
                    list_state.set_offset_from_scrollbar(point(px(0.0), -scroll_position));
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
            | (ScrollbarAxis::Vertical, Some(ScrollbarDrag::Vertical { pointer_offset })) => {
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

fn render_body_row(
    workbook: &WorkbookData,
    row_ix: usize,
    horizontal_offset: Pixels,
) -> AnyElement {
    let row_height = workbook.row_height(row_ix);

    div()
        .id(("sheet-row", row_ix))
        .flex()
        .h(px(row_height))
        .w_full()
        .child(row_header(row_ix, row_height))
        .child(
            div().flex_1().h(px(row_height)).overflow_hidden().child(
                render_cells_row(workbook, row_ix, row_height)
                    .relative()
                    .left(horizontal_offset),
            ),
        )
        .into_any_element()
}

fn render_cells_row(workbook: &WorkbookData, row_ix: usize, row_height: f32) -> Stateful<Div> {
    let mut row = div()
        .id(("sheet-cells-row", row_ix))
        .flex()
        .h(px(row_height))
        .w(px(workbook.sheet_width()));

    for col_ix in 0..workbook.col_count() {
        row = row.child(cell(
            workbook.cell_data(row_ix, col_ix),
            workbook.column_width(col_ix),
            row_height,
        ));
    }

    row
}

fn corner_header() -> Div {
    div()
        .w(px(ROW_HEADER_WIDTH))
        .h(px(HEADER_HEIGHT))
        .flex_none()
        .bg(rgb(HEADER_BG))
        .border_1()
        .border_color(rgb(GRID_COLOR))
}

fn column_headers(workbook: &WorkbookData) -> Div {
    let mut row = div()
        .flex()
        .h(px(HEADER_HEIGHT))
        .w(px(workbook.sheet_width()));

    for col_ix in 0..workbook.col_count() {
        row = row.child(column_header(
            column_name(col_ix),
            workbook.column_width(col_ix),
        ));
    }

    row
}

fn column_header(label: String, width: f32) -> Div {
    div()
        .w(px(width))
        .h(px(HEADER_HEIGHT))
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .overflow_hidden()
        .whitespace_nowrap()
        .bg(rgb(HEADER_BG))
        .border_1()
        .border_l_0()
        .border_color(rgb(GRID_COLOR))
        .text_color(rgb(HEADER_TEXT))
        .child(label)
}

fn row_header(row_ix: usize, row_height: f32) -> Div {
    div()
        .w(px(ROW_HEADER_WIDTH))
        .h(px(row_height))
        .flex()
        .items_center()
        .justify_end()
        .flex_none()
        .px_2()
        .overflow_hidden()
        .whitespace_nowrap()
        .bg(rgb(HEADER_BG))
        .border_1()
        .border_t_0()
        .border_color(rgb(GRID_COLOR))
        .text_color(rgb(HEADER_TEXT))
        .child((row_ix + 1).to_string())
}

fn cell(cell: CellData, width: f32, row_height: f32) -> Div {
    let mut element = div()
        .w(px(width))
        .h(px(row_height))
        .flex()
        .items_center()
        .flex_none()
        .px_2()
        .overflow_hidden()
        .whitespace_nowrap()
        .border_1()
        .border_l_0()
        .border_t_0()
        .border_color(rgb(GRID_COLOR))
        .bg(rgb(cell.style.background_color.unwrap_or(CELL_BG)))
        .text_color(rgb(cell.style.text_color.unwrap_or(CELL_TEXT)));

    if cell.style.bold {
        element = element.font_weight(FontWeight::BOLD);
    }

    element.child(cell.value)
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
}
