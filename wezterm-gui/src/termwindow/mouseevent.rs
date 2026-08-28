use crate::tabbar::TabBarItem;
use crate::termwindow::{
    DragDropSource, GuiWin, MouseCapture, PaneDragState, PositionedSplit, ScrollHit, TabDragState,
    TermWindowNotif, UIItem, UIItemType, TMB,
};
use ::window::{
    Connection, ConnectionOps, MouseButtons as WMB, MouseCursor, MouseEvent,
    MouseEventKind as WMEK, MousePress, Point, RectF, ScreenPoint, Size, WindowDecorations,
    WindowOps, WindowState,
};
use config::keyassignment::{KeyAssignment, MouseEventTrigger, SpawnTabDomain};
use config::MouseEventAltScreen;
use mux::pane::{Pane, WithPaneLines};
use mux::tab::{SplitDirection, SplitRequest, SplitSize};
use mux::Mux;
use mux_lua::MuxPane;
use std::convert::TryInto;
use std::ops::Sub;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use termwiz::hyperlink::Hyperlink;
use termwiz::surface::Line;
use wezterm_dynamic::ToDynamic;
use wezterm_term::input::{MouseButton, MouseEventKind as TMEK};
use wezterm_term::{ClickPosition, LastMouseClick, StableRowIndex};

const DRAG_THRESHOLD_PIXELS: isize = 6;
const DROP_EDGE_FRACTION: f32 = 0.25;
const DROP_SPLIT_PERCENT: u8 = 50;
const TAB_BAR_DROP_MARKER_WIDTH_PIXELS: f32 = 4.0;

impl super::TermWindow {
    fn resolve_ui_item(&self, event: &MouseEvent) -> Option<UIItem> {
        self.resolve_ui_item_at(event.coords)
    }

    fn resolve_ui_item_at(&self, point: Point) -> Option<UIItem> {
        self.ui_items
            .iter()
            .rev()
            .find(|item| item.hit_test(point.x, point.y))
            .cloned()
    }

    fn leave_ui_item(&mut self, item: &UIItem) {
        match item.item_type {
            UIItemType::TabBar(_) => {
                self.update_title_post_status();
            }
            UIItemType::CloseTab(_)
            | UIItemType::AboveScrollThumb
            | UIItemType::BelowScrollThumb
            | UIItemType::ScrollThumb
            | UIItemType::Split(_)
            | UIItemType::PaneDragHandle(_) => {}
        }
    }

    fn enter_ui_item(&mut self, item: &UIItem) {
        match item.item_type {
            UIItemType::TabBar(_) => {}
            UIItemType::CloseTab(_)
            | UIItemType::AboveScrollThumb
            | UIItemType::BelowScrollThumb
            | UIItemType::ScrollThumb
            | UIItemType::Split(_)
            | UIItemType::PaneDragHandle(_) => {}
        }
    }

    pub fn mouse_event_impl(&mut self, event: MouseEvent, context: &dyn WindowOps) {
        log::trace!("{:?}", event);
        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };

        self.current_mouse_event.replace(event.clone());

        let border = self.get_os_border();

        let first_line_offset = if self.show_tab_bar && !self.config.tab_bar_at_bottom {
            self.tab_bar_pixel_height().unwrap_or(0.) as isize
        } else {
            0
        } + border.top.get() as isize;

        let (padding_left, padding_top) = self.padding_left_top();

        let y = (event
            .coords
            .y
            .sub(padding_top as isize)
            .sub(first_line_offset)
            .max(0)
            / self.render_metrics.cell_size.height) as i64;

        let x = (event
            .coords
            .x
            .sub((padding_left + border.left.get() as f32) as isize)
            .max(0) as f32)
            / self.render_metrics.cell_size.width as f32;
        let x = if !pane.is_mouse_grabbed() {
            // Round the x coordinate so that we're a bit more forgiving of
            // the horizontal position when selecting cells
            x.round()
        } else {
            x
        }
        .trunc() as usize;

        let mut y_pixel_offset = event
            .coords
            .y
            .sub(padding_top as isize)
            .sub(first_line_offset);
        if y > 0 {
            y_pixel_offset = y_pixel_offset.max(0) % self.render_metrics.cell_size.height;
        }

        let mut x_pixel_offset = event
            .coords
            .x
            .sub((padding_left + border.left.get() as f32) as isize);
        if x > 0 {
            x_pixel_offset = x_pixel_offset.max(0) % self.render_metrics.cell_size.width;
        }

        self.last_mouse_coords = (x, y);

        let mut capture_mouse = false;

        match event.kind {
            WMEK::Release(ref press) => {
                self.current_mouse_capture = None;
                self.current_mouse_buttons.retain(|p| p != press);
                if press == &MousePress::Left {
                    if let Some(pane_drag) = self.pane_drag.take() {
                        if pane_drag.started {
                            self.finish_pane_drag(pane_drag, &event);
                        }
                        context.set_cursor(Some(MouseCursor::Arrow));
                        context.invalidate();
                        return;
                    }
                    if let Some(tab_drag) = self.tab_drag.take() {
                        if tab_drag.started {
                            self.finish_tab_drag(tab_drag, &event);
                        }
                        context.set_cursor(Some(MouseCursor::Arrow));
                        context.invalidate();
                        return;
                    }
                }
                if press == &MousePress::Left && self.window_drag_position.take().is_some() {
                    // Completed a window drag
                    return;
                }
                if press == &MousePress::Left && self.dragging.take().is_some() {
                    // Completed a drag
                    return;
                }
            }

            WMEK::Press(ref press) => {
                capture_mouse = true;

                // Perform click counting
                let button = mouse_press_to_tmb(press);

                let click_position = ClickPosition {
                    column: x,
                    row: y,
                    x_pixel_offset,
                    y_pixel_offset,
                };

                let click = match self.last_mouse_click.take() {
                    None => LastMouseClick::new(button, click_position),
                    Some(click) => click.add(button, click_position),
                };
                self.last_mouse_click = Some(click);
                self.current_mouse_buttons.retain(|p| p != press);
                self.current_mouse_buttons.push(*press);
            }

            WMEK::Move => {
                if let Some(pane_drag) = self.pane_drag.take() {
                    self.drag_pane(pane_drag, &event, context);
                    return;
                }
                if let Some(tab_drag) = self.tab_drag.take() {
                    self.drag_tab(tab_drag, &event, context);
                    return;
                }

                if let Some(start) = self.window_drag_position.as_ref() {
                    // Dragging the window
                    // Compute the distance since the initial event
                    let delta_x = start.screen_coords.x - event.screen_coords.x;
                    let delta_y = start.screen_coords.y - event.screen_coords.y;

                    // Now compute a new window position.
                    // We don't have a direct way to get the position,
                    // but we can infer it by comparing the mouse coords
                    // with the screen coords in the initial event.
                    // This computes the original top_left position,
                    // and applies the total drag delta to it.
                    let top_left = ::window::ScreenPoint::new(
                        (start.screen_coords.x - start.coords.x) - delta_x,
                        (start.screen_coords.y - start.coords.y) - delta_y,
                    );
                    // and now tell the window to go there
                    context.set_window_position(top_left);
                    return;
                }

                if let Some((item, start_event)) = self.dragging.take() {
                    self.drag_ui_item(item, start_event, x, y, event, context);
                    return;
                }
            }
            _ => {}
        }

        let prior_ui_item = self.last_ui_item.clone();

        let ui_item = if matches!(self.current_mouse_capture, None | Some(MouseCapture::UI)) {
            let ui_item = self.resolve_ui_item(&event);

            match (self.last_ui_item.take(), &ui_item) {
                (Some(prior), Some(item)) => {
                    if prior != *item || !self.config.use_fancy_tab_bar {
                        self.leave_ui_item(&prior);
                        self.enter_ui_item(item);
                        context.invalidate();
                    }
                }
                (Some(prior), None) => {
                    self.leave_ui_item(&prior);
                    context.invalidate();
                }
                (None, Some(item)) => {
                    self.enter_ui_item(item);
                    context.invalidate();
                }
                (None, None) => {}
            }

            ui_item
        } else {
            None
        };

        if let Some(item) = ui_item.clone() {
            if capture_mouse {
                self.current_mouse_capture = Some(MouseCapture::UI);
            }
            self.mouse_event_ui_item(item, pane, y, event, context);
        } else if matches!(
            self.current_mouse_capture,
            None | Some(MouseCapture::TerminalPane(_))
        ) {
            self.mouse_event_terminal(
                pane,
                ClickPosition {
                    column: x,
                    row: y,
                    x_pixel_offset,
                    y_pixel_offset,
                },
                event,
                context,
                capture_mouse,
            );
        }

        if prior_ui_item != ui_item {
            self.update_title_post_status();
        }
    }

    pub fn mouse_leave_impl(&mut self, context: &dyn WindowOps) {
        self.current_mouse_event = None;
        self.update_title();
        context.set_cursor(Some(MouseCursor::Arrow));
        context.invalidate();
    }

    fn mouse_event_pane_drag_handle(
        &mut self,
        pane_id: mux::pane::PaneId,
        item: UIItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(MouseCursor::Hand));
        if event.kind != WMEK::Press(MousePress::Left) {
            return;
        }

        let mux = Mux::get();
        let Some((_domain_id, source_window_id, source_tab_id)) = mux.resolve_pane_id(pane_id)
        else {
            log::error!("cannot start dragging pane {pane_id}: pane is not attached to the mux");
            return;
        };
        if source_window_id != self.mux_window_id {
            log::error!(
                "cannot start dragging pane {pane_id}: pane belongs to window {source_window_id}, not GUI window {}",
                self.mux_window_id
            );
            return;
        }
        if let Some(source_tab) = mux.get_tab(source_tab_id) {
            if let Some(source_index) = source_tab
                .iter_panes_ignoring_zoom()
                .into_iter()
                .find(|positioned| positioned.pane.pane_id() == pane_id)
                .map(|positioned| positioned.index)
            {
                source_tab.set_active_idx(source_index);
            }
        }

        let same_window_target_tab_id = mux.get_window(self.mux_window_id).and_then(|window| {
            window
                .get_last_active_idx()
                .and_then(|idx| window.get_by_idx(idx))
                .map(|tab| tab.tab_id())
                .filter(|target_id| *target_id != source_tab_id)
                .or_else(|| {
                    window
                        .iter()
                        .find(|tab| tab.tab_id() != source_tab_id)
                        .map(|tab| tab.tab_id())
                })
        });
        self.pane_drag = Some(PaneDragState {
            pane_id,
            source_tab_id,
            start_event: event.clone(),
            grab_offset: Point::new(
                event.coords.x.saturating_sub(item.x as isize),
                event.coords.y.saturating_sub(item.y as isize),
            ),
            dragged_size: Size::new(item.width as isize, item.height as isize),
            current_coords: event.coords,
            preview_window: None,
            same_window_target_tab_id,
            started: false,
        });
    }

    fn drag_pane(
        &mut self,
        mut pane_drag: PaneDragState,
        event: &MouseEvent,
        context: &dyn WindowOps,
    ) {
        pane_drag.current_coords = event.coords;
        if !pane_drag.started {
            let delta_x = (event.screen_coords.x - pane_drag.start_event.screen_coords.x).abs();
            let delta_y = (event.screen_coords.y - pane_drag.start_event.screen_coords.y).abs();
            pane_drag.started = delta_x.max(delta_y) >= DRAG_THRESHOLD_PIXELS;
        }
        if !pane_drag.started {
            self.pane_drag = Some(pane_drag);
            return;
        }

        context.set_cursor(Some(MouseCursor::Hand));
        let source = DragDropSource::Pane {
            pane_id: pane_drag.pane_id,
            tab_id: pane_drag.source_tab_id,
        };
        let supports_cross_window_drag = Connection::get()
            .map(|connection| connection.supports_cross_window_tab_drag())
            .unwrap_or(false);
        let mut preview_window = None;
        let mut source_client_point = None;
        if supports_cross_window_drag {
            if let Some((target, client_point)) =
                crate::frontend::front_end().gui_window_at_screen_point(event.screen_coords)
            {
                if target.mux_window_id == self.mux_window_id {
                    source_client_point = Some(client_point);
                } else {
                    target
                        .window
                        .notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                            term_window.update_drop_preview(source, client_point);
                        })));
                    preview_window = Some(target.window);
                }
            }
        } else if self.point_is_in_client(event.coords) {
            source_client_point = Some(event.coords);
        }

        if pane_drag.preview_window != preview_window {
            if let Some(window) = pane_drag.preview_window.take() {
                window.notify(TermWindowNotif::Apply(Box::new(|term_window| {
                    term_window.clear_drag_drop_preview();
                })));
            }
            pane_drag.preview_window = preview_window;
        }

        if let Some(point) = source_client_point {
            let over_tab_bar = self
                .tab_bar_drop_bounds()
                .map(|bounds| Self::point_in_rect(point, bounds))
                .unwrap_or(false);
            if !over_tab_bar {
                if let Some(target_tab_id) = pane_drag.same_window_target_tab_id {
                    if let Some(mut window) = Mux::get().get_window_mut(self.mux_window_id) {
                        if let Some(target_index) = window.idx_by_id(target_tab_id) {
                            window.set_active_without_saving(target_index);
                        }
                    }
                }
            }
            self.update_drop_preview(source, point);
        } else {
            self.clear_drag_drop_preview();
        }

        context.invalidate();
        self.pane_drag = Some(pane_drag);
    }

    fn finish_pane_drag(&mut self, mut pane_drag: PaneDragState, event: &MouseEvent) {
        if let Some(window) = pane_drag.preview_window.take() {
            window.notify(TermWindowNotif::Apply(Box::new(|term_window| {
                term_window.clear_drag_drop_preview();
            })));
        }
        self.clear_drag_drop_preview();

        let source = DragDropSource::Pane {
            pane_id: pane_drag.pane_id,
            tab_id: pane_drag.source_tab_id,
        };
        let supports_cross_window_drag = Connection::get()
            .map(|connection| connection.supports_cross_window_tab_drag())
            .unwrap_or(false);
        let target = if supports_cross_window_drag {
            crate::frontend::front_end().gui_window_at_screen_point(event.screen_coords)
        } else {
            None
        };

        match target {
            Some((target, client_point)) if target.mux_window_id == self.mux_window_id => {
                self.finish_local_pane_drop(source, client_point);
            }
            Some((target, client_point)) => {
                let pane_id = pane_drag.pane_id;
                let source_tab_id = pane_drag.source_tab_id;
                target
                    .window
                    .notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                        term_window.accept_pane_drop(pane_id, source_tab_id, client_point);
                    })));
            }
            None if !supports_cross_window_drag && self.point_is_in_client(event.coords) => {
                self.finish_local_pane_drop(source, event.coords);
            }
            None if supports_cross_window_drag => {
                Self::detach_pane_to_new_window(
                    pane_drag.pane_id,
                    pane_drag.source_tab_id,
                    event.screen_coords,
                    pane_drag.grab_offset,
                );
            }
            None => Self::restore_drag_source_tab(pane_drag.source_tab_id),
        }
    }

    fn finish_local_pane_drop(&mut self, source: DragDropSource, client_point: Point) {
        if self.drop_preview_at(source, client_point).is_some() {
            if let DragDropSource::Pane { pane_id, tab_id } = source {
                self.accept_pane_drop(pane_id, tab_id, client_point);
            }
        } else {
            Self::restore_drag_source_tab(source.tab_id());
        }
    }

    fn restore_drag_source_tab(source_tab_id: mux::tab::TabId) {
        let mux = Mux::get();
        if let Some(source_window_id) = mux.window_containing_tab(source_tab_id) {
            if let Some(mut window) = mux.get_window_mut(source_window_id) {
                if let Some(source_index) = window.idx_by_id(source_tab_id) {
                    window.set_active_without_saving(source_index);
                }
            }
        }
    }

    fn point_is_in_client(&self, point: Point) -> bool {
        point.x >= 0
            && point.y >= 0
            && point.x < self.dimensions.pixel_width as isize
            && point.y < self.dimensions.pixel_height as isize
    }

    fn drag_tab(
        &mut self,
        mut tab_drag: TabDragState,
        event: &MouseEvent,
        context: &dyn WindowOps,
    ) {
        tab_drag.current_coords = event.coords;
        if !tab_drag.started {
            let delta_x = (event.screen_coords.x - tab_drag.start_event.screen_coords.x).abs();
            let delta_y = (event.screen_coords.y - tab_drag.start_event.screen_coords.y).abs();
            tab_drag.started = delta_x.max(delta_y) >= DRAG_THRESHOLD_PIXELS;
        }

        if tab_drag.started {
            context.set_cursor(Some(MouseCursor::Hand));
            let mut preview_window = None;
            let mut source_client_point = None;
            if Connection::get()
                .map(|connection| connection.supports_cross_window_tab_drag())
                .unwrap_or(false)
            {
                if let Some((target, client_point)) =
                    crate::frontend::front_end().gui_window_at_screen_point(event.screen_coords)
                {
                    if target.mux_window_id != self.mux_window_id {
                        let tab_id = tab_drag.tab_id;
                        target.window.notify(TermWindowNotif::Apply(Box::new(
                            move |term_window| {
                                term_window
                                    .update_drop_preview(DragDropSource::Tab(tab_id), client_point);
                            },
                        )));
                        preview_window = Some(target.window);
                    } else {
                        source_client_point = Some(client_point);
                    }
                }
            }

            if tab_drag.preview_window != preview_window {
                if let Some(window) = tab_drag.preview_window.take() {
                    window.notify(TermWindowNotif::Apply(Box::new(|term_window| {
                        term_window.clear_drag_drop_preview();
                    })));
                }
                tab_drag.preview_window = preview_window;
            }

            let over_source_tab_bar = source_client_point
                .and_then(|point| self.tab_bar_drop_bounds().map(|bounds| (point, bounds)))
                .map(|(point, bounds)| Self::point_in_rect(point, bounds))
                .unwrap_or(false);
            if let (Some(point), Some(target_tab_id)) =
                (source_client_point, tab_drag.same_window_target_tab_id)
            {
                if !over_source_tab_bar {
                    if let Some(mut window) = Mux::get().get_window_mut(self.mux_window_id) {
                        if let Some(target_index) = window.idx_by_id(target_tab_id) {
                            window.set_active_without_saving(target_index);
                        }
                    }
                    self.update_drop_preview(DragDropSource::Tab(tab_drag.tab_id), point);
                } else {
                    self.clear_drag_drop_preview();
                }
            } else {
                self.clear_drag_drop_preview();
            }

            if tab_drag.preview_window.is_none() && self.drag_drop_preview.is_none() {
                if let Some(UIItem {
                    item_type: UIItemType::TabBar(TabBarItem::Tab { tab_idx, .. }),
                    ..
                }) = self.resolve_ui_item(event)
                {
                    if let Err(err) =
                        Mux::get().move_tab_to_window(tab_drag.tab_id, self.mux_window_id, tab_idx)
                    {
                        log::error!(
                            "reordering dragged tab {} in window {}: {err:#}",
                            tab_drag.tab_id,
                            self.mux_window_id
                        );
                    }
                }
            }
            context.invalidate();
        }

        self.tab_drag.replace(tab_drag);
    }

    fn finish_tab_drag(&mut self, mut tab_drag: TabDragState, event: &MouseEvent) {
        if let Some(window) = tab_drag.preview_window.take() {
            window.notify(TermWindowNotif::Apply(Box::new(|term_window| {
                term_window.clear_drag_drop_preview();
            })));
        }
        self.clear_drag_drop_preview();

        let Some(connection) = Connection::get() else {
            return;
        };
        if !connection.supports_cross_window_tab_drag() {
            return;
        }

        let grab_offset = tab_drag.grab_offset;
        match crate::frontend::front_end().gui_window_at_screen_point(event.screen_coords) {
            Some((target, client_point)) if target.mux_window_id == self.mux_window_id => {
                if self
                    .drop_preview_at(DragDropSource::Tab(tab_drag.tab_id), client_point)
                    .is_some()
                {
                    self.accept_tab_drop(
                        tab_drag.tab_id,
                        client_point,
                        event.screen_coords,
                        grab_offset,
                    );
                } else if let Some(mut window) = Mux::get().get_window_mut(self.mux_window_id) {
                    if let Some(source_index) = window.idx_by_id(tab_drag.tab_id) {
                        window.set_active_without_saving(source_index);
                    }
                }
            }
            Some((target, client_point)) => {
                let tab_id = tab_drag.tab_id;
                let screen_point = event.screen_coords;
                target
                    .window
                    .notify(TermWindowNotif::Apply(Box::new(move |term_window| {
                        term_window.accept_tab_drop(
                            tab_id,
                            client_point,
                            screen_point,
                            grab_offset,
                        );
                    })));
            }
            None => {
                Self::detach_tab_to_new_window(tab_drag.tab_id, event.screen_coords, grab_offset);
            }
        }
    }

    fn accept_tab_drop(
        &mut self,
        tab_id: mux::tab::TabId,
        client_point: Point,
        screen_point: ScreenPoint,
        grab_offset: Point,
    ) {
        let mux = Mux::get();
        match self.drop_preview_at(DragDropSource::Tab(tab_id), client_point) {
            Some(super::DragDropPreview {
                action: super::DragDropAction::Merge { destination_index },
                ..
            }) => {
                if let Err(err) =
                    mux.move_tab_to_window(tab_id, self.mux_window_id, destination_index)
                {
                    log::error!(
                        "moving dragged tab {tab_id} into window {} at index {destination_index}: {err:#}",
                        self.mux_window_id
                    );
                }
            }
            Some(super::DragDropPreview {
                action:
                    super::DragDropAction::Split {
                        target_pane_id,
                        request,
                    },
                ..
            }) => {
                if let Err(err) = mux.move_single_pane_tab_to_split(tab_id, target_pane_id, request)
                {
                    log::error!(
                        "splitting dragged tab {tab_id} onto pane {target_pane_id}: {err:#}"
                    );
                    let destination_index = mux
                        .get_window(self.mux_window_id)
                        .map(|window| window.len())
                        .unwrap_or(0);
                    if let Err(fallback_err) =
                        mux.move_tab_to_window(tab_id, self.mux_window_id, destination_index)
                    {
                        log::error!(
                            "falling back to merging dragged tab {tab_id} into window {} after split failed: {fallback_err:#}",
                            self.mux_window_id
                        );
                    }
                }
            }
            None => Self::detach_tab_to_new_window(tab_id, screen_point, grab_offset),
        }
    }

    fn accept_pane_drop(
        &mut self,
        pane_id: mux::pane::PaneId,
        source_tab_id: mux::tab::TabId,
        client_point: Point,
    ) {
        let source = DragDropSource::Pane {
            pane_id,
            tab_id: source_tab_id,
        };
        match self.drop_preview_at(source, client_point) {
            Some(super::DragDropPreview {
                action: super::DragDropAction::Merge { destination_index },
                ..
            }) => Self::move_pane_to_tab(
                pane_id,
                source_tab_id,
                self.mux_window_id,
                destination_index,
            ),
            Some(super::DragDropPreview {
                action:
                    super::DragDropAction::Split {
                        target_pane_id,
                        request,
                    },
                ..
            }) => {
                if let Err(err) = Mux::get().move_pane_to_split(pane_id, target_pane_id, request) {
                    log::error!(
                        "moving dragged pane {pane_id} beside target pane {target_pane_id}: {err:#}"
                    );
                }
            }
            None => Self::restore_drag_source_tab(source_tab_id),
        }
    }

    fn move_pane_to_tab(
        pane_id: mux::pane::PaneId,
        source_tab_id: mux::tab::TabId,
        destination_window_id: mux::window::WindowId,
        destination_index: usize,
    ) {
        let mux = Mux::get();
        let source_is_single_pane = mux
            .get_tab(source_tab_id)
            .map(|tab| tab.iter_panes_ignoring_zoom().len() == 1)
            .unwrap_or(false);
        if source_is_single_pane {
            if let Err(err) =
                mux.move_tab_to_window(source_tab_id, destination_window_id, destination_index)
            {
                log::error!(
                    "moving single-pane tab {source_tab_id} for dragged pane {pane_id} into window {destination_window_id} at index {destination_index}: {err:#}"
                );
            }
            return;
        }

        promise::spawn::spawn(async move {
            let moved = mux
                .move_pane_to_new_tab(pane_id, Some(destination_window_id), None)
                .await;
            match moved {
                Ok((tab, actual_window_id)) => {
                    if let Err(err) =
                        mux.move_tab_to_window(tab.tab_id(), actual_window_id, destination_index)
                    {
                        log::error!(
                            "placing new tab {} for dragged pane {pane_id} in window {actual_window_id} at index {destination_index}: {err:#}",
                            tab.tab_id()
                        );
                    }
                    if let Err(err) = mux.focus_pane_and_containing_tab(pane_id) {
                        log::error!("focusing pane {pane_id} after moving it to a tab: {err:#}");
                    }
                }
                Err(err) => {
                    log::error!(
                        "moving pane {pane_id} from tab {source_tab_id} into a new tab in window {destination_window_id}: {err:#}"
                    );
                }
            }
        })
        .detach();
    }

    fn detach_pane_to_new_window(
        pane_id: mux::pane::PaneId,
        source_tab_id: mux::tab::TabId,
        point: ScreenPoint,
        grab_offset: Point,
    ) {
        let mux = Mux::get();
        let source_is_single_pane = mux
            .get_tab(source_tab_id)
            .map(|tab| tab.iter_panes_ignoring_zoom().len() == 1)
            .unwrap_or(false);
        if source_is_single_pane {
            let position = config::GuiPosition {
                x: config::Dimension::Pixels((point.x - grab_offset.x) as f32),
                y: config::Dimension::Pixels((point.y - grab_offset.y) as f32),
                origin: config::GeometryOrigin::ScreenCoordinateSystem,
            };
            if let Err(err) = mux.move_tab_to_new_window(source_tab_id, Some(position)) {
                log::error!(
                    "detaching single-pane tab {source_tab_id} for pane {pane_id} into a new window at ({}, {}): {err:#}",
                    point.x,
                    point.y
                );
            }
            return;
        }

        promise::spawn::spawn(async move {
            match mux.move_pane_to_new_tab(pane_id, None, None).await {
                Ok((_tab, _window_id)) => {
                    if let Err(err) = mux.focus_pane_and_containing_tab(pane_id) {
                        log::error!(
                            "focusing pane {pane_id} after detaching it into a new window: {err:#}"
                        );
                    }
                }
                Err(err) => {
                    log::error!(
                        "detaching pane {pane_id} from tab {source_tab_id} into a new window: {err:#}"
                    );
                }
            }
        })
        .detach();
    }

    fn tab_drop_index(&self, point: Point) -> Option<usize> {
        let item = self.resolve_ui_item_at(point)?;
        match item.item_type {
            UIItemType::TabBar(TabBarItem::Tab { tab_idx, .. }) => {
                let midpoint = item.x.saturating_add(item.width / 2) as isize;
                Some(tab_idx + usize::from(point.x > midpoint))
            }
            UIItemType::TabBar(
                TabBarItem::None
                | TabBarItem::LeftStatus
                | TabBarItem::RightStatus
                | TabBarItem::NewTabButton,
            ) => Mux::get()
                .get_window(self.mux_window_id)
                .map(|window| window.len()),
            UIItemType::TabBar(TabBarItem::WindowButton(_))
            | UIItemType::CloseTab(_)
            | UIItemType::AboveScrollThumb
            | UIItemType::BelowScrollThumb
            | UIItemType::ScrollThumb
            | UIItemType::Split(_)
            | UIItemType::PaneDragHandle(_) => None,
        }
    }

    fn tab_bar_drop_bounds(&self) -> Option<RectF> {
        if !self.show_tab_bar {
            return None;
        }
        let height = self.tab_bar_pixel_height().ok()?;
        let border = self.get_os_border();
        let y = if self.config.tab_bar_at_bottom {
            (self.dimensions.pixel_height as f32 - height - border.bottom.get() as f32).max(0.0)
        } else {
            border.top.get() as f32
        };
        Some(euclid::rect(
            0.0,
            y,
            self.dimensions.pixel_width as f32,
            height,
        ))
    }

    fn point_in_rect(point: Point, rect: RectF) -> bool {
        point.x as f32 >= rect.min_x()
            && point.x as f32 <= rect.max_x()
            && point.y as f32 >= rect.min_y()
            && point.y as f32 <= rect.max_y()
    }

    fn drop_preview_at(
        &self,
        source: DragDropSource,
        point: Point,
    ) -> Option<super::DragDropPreview> {
        let source_tab_id = source.tab_id();
        if let Some(tab_bar_bounds) = self.tab_bar_drop_bounds() {
            if Self::point_in_rect(point, tab_bar_bounds) {
                let destination_index = self.tab_drop_index(point).or_else(|| {
                    Mux::get()
                        .get_window(self.mux_window_id)
                        .map(|window| window.len())
                })?;
                let marker_x = self
                    .ui_items
                    .iter()
                    .filter_map(|item| match item.item_type {
                        UIItemType::TabBar(TabBarItem::Tab { tab_idx, .. })
                            if tab_idx == destination_index =>
                        {
                            Some(item.x as f32)
                        }
                        _ => None,
                    })
                    .next()
                    .or_else(|| {
                        self.ui_items
                            .iter()
                            .filter_map(|item| match item.item_type {
                                UIItemType::TabBar(TabBarItem::Tab { .. }) => {
                                    Some((item.x + item.width) as f32)
                                }
                                _ => None,
                            })
                            .max_by(f32::total_cmp)
                    })
                    .unwrap_or(tab_bar_bounds.min_x());
                let marker_x = marker_x.clamp(
                    tab_bar_bounds.min_x(),
                    (tab_bar_bounds.max_x() - TAB_BAR_DROP_MARKER_WIDTH_PIXELS).max(0.0),
                );
                return Some(super::DragDropPreview {
                    action: super::DragDropAction::Merge { destination_index },
                    rect: euclid::rect(
                        marker_x,
                        tab_bar_bounds.min_y(),
                        TAB_BAR_DROP_MARKER_WIDTH_PIXELS,
                        tab_bar_bounds.height(),
                    ),
                });
            }
        }

        let mux = Mux::get();
        let source_is_in_destination_window =
            mux.window_containing_tab(source_tab_id) == Some(self.mux_window_id);
        let source_can_split = match source {
            DragDropSource::Tab(tab_id) => mux
                .get_tab(tab_id)
                .map(|tab| tab.iter_panes_ignoring_zoom().len() == 1)
                .unwrap_or(false),
            DragDropSource::Pane { .. } => true,
        };
        let destination_tab = mux.get_active_tab_for_window(self.mux_window_id)?;
        if destination_tab.tab_id() == source_tab_id {
            return None;
        }
        let destination_index = mux
            .get_window(self.mux_window_id)
            .map(|window| window.len())?;

        for positioned in destination_tab.iter_panes() {
            let bounds = self.pane_drag_bounds(&positioned);
            if bounds.width() <= 0.0
                || bounds.height() <= 0.0
                || !Self::point_in_rect(point, bounds)
            {
                continue;
            }

            if source_can_split {
                let x_fraction = (point.x as f32 - bounds.min_x()) / bounds.width();
                let y_fraction = (point.y as f32 - bounds.min_y()) / bounds.height();
                let (edge_distance, direction, target_is_second) = {
                    let mut candidate = (x_fraction, SplitDirection::Horizontal, false);
                    if 1.0 - x_fraction < candidate.0 {
                        candidate = (1.0 - x_fraction, SplitDirection::Horizontal, true);
                    }
                    if y_fraction < candidate.0 {
                        candidate = (y_fraction, SplitDirection::Vertical, false);
                    }
                    if 1.0 - y_fraction < candidate.0 {
                        candidate = (1.0 - y_fraction, SplitDirection::Vertical, true);
                    }
                    candidate
                };

                if edge_distance <= DROP_EDGE_FRACTION {
                    let request = SplitRequest {
                        direction,
                        target_is_second,
                        top_level: false,
                        size: SplitSize::Percent(DROP_SPLIT_PERCENT),
                    };
                    let split_is_valid = destination_tab.get_zoomed_pane().is_none()
                        && destination_tab
                            .compute_split_size(positioned.index, request)
                            .map(|size| {
                                size.first.rows > 0
                                    && size.first.cols > 0
                                    && size.second.rows > 0
                                    && size.second.cols > 0
                            })
                            .unwrap_or(false);
                    if split_is_valid {
                        let mut preview = bounds;
                        match (direction, target_is_second) {
                            (SplitDirection::Horizontal, false) => preview.size.width /= 2.0,
                            (SplitDirection::Horizontal, true) => {
                                preview.origin.x += preview.size.width / 2.0;
                                preview.size.width /= 2.0;
                            }
                            (SplitDirection::Vertical, false) => preview.size.height /= 2.0,
                            (SplitDirection::Vertical, true) => {
                                preview.origin.y += preview.size.height / 2.0;
                                preview.size.height /= 2.0;
                            }
                        }
                        return Some(super::DragDropPreview {
                            action: super::DragDropAction::Split {
                                target_pane_id: positioned.pane.pane_id(),
                                request,
                            },
                            rect: preview,
                        });
                    }
                }
            }

            return match source {
                DragDropSource::Pane { .. } => None,
                DragDropSource::Tab(_) if source_is_in_destination_window => None,
                DragDropSource::Tab(_) => Some(super::DragDropPreview {
                    action: super::DragDropAction::Merge { destination_index },
                    rect: bounds,
                }),
            };
        }
        None
    }

    fn update_drop_preview(&mut self, source: DragDropSource, point: Point) {
        let preview = self.drop_preview_at(source, point);
        if self.drag_drop_preview != preview {
            self.drag_drop_preview = preview;
            if let Some(window) = self.window.as_ref() {
                window.invalidate();
            }
        }
    }

    fn clear_drag_drop_preview(&mut self) {
        if self.drag_drop_preview.take().is_some() {
            if let Some(window) = self.window.as_ref() {
                window.invalidate();
            }
        }
    }

    fn detach_tab_to_new_window(tab_id: mux::tab::TabId, point: ScreenPoint, grab_offset: Point) {
        let position = config::GuiPosition {
            x: config::Dimension::Pixels((point.x - grab_offset.x) as f32),
            y: config::Dimension::Pixels((point.y - grab_offset.y) as f32),
            origin: config::GeometryOrigin::ScreenCoordinateSystem,
        };
        if let Err(err) = Mux::get().move_tab_to_new_window(tab_id, Some(position)) {
            log::error!(
                "detaching dragged tab {tab_id} into a new window at ({}, {}): {err:#}",
                point.x,
                point.y
            );
        }
    }

    fn drag_split(
        &mut self,
        mut item: UIItem,
        split: PositionedSplit,
        start_event: MouseEvent,
        x: usize,
        y: i64,
        context: &dyn WindowOps,
    ) {
        let mux = Mux::get();
        let tab = match mux.get_active_tab_for_window(self.mux_window_id) {
            Some(tab) => tab,
            None => return,
        };
        let delta = match split.direction {
            SplitDirection::Horizontal => (x as isize).saturating_sub(split.left as isize),
            SplitDirection::Vertical => (y as isize).saturating_sub(split.top as isize),
        };

        if delta != 0 {
            tab.resize_split_by(split.index, delta);
            if let Some(split) = tab.iter_splits().into_iter().nth(split.index) {
                item.item_type = UIItemType::Split(split);
                context.invalidate();
            }
        }
        self.dragging.replace((item, start_event));
    }

    fn drag_scroll_thumb(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };

        let dims = pane.get_dimensions();
        let current_viewport = self.get_viewport(pane.pane_id());

        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height().unwrap_or(0.)
        } else {
            0.
        };
        let (top_bar_height, bottom_bar_height) = if self.config.tab_bar_at_bottom {
            (0.0, tab_bar_height)
        } else {
            (tab_bar_height, 0.0)
        };

        let border = self.get_os_border();
        let y_offset = top_bar_height + border.top.get() as f32;

        let from_top = start_event.coords.y.saturating_sub(item.y as isize);
        let effective_thumb_top = event
            .coords
            .y
            .saturating_sub(y_offset as isize + from_top)
            .max(0) as usize;

        // Convert thumb top into a row index by reversing the math
        // in ScrollHit::thumb
        let row = ScrollHit::thumb_top_to_scroll_top(
            effective_thumb_top,
            &*pane,
            current_viewport,
            self.dimensions.pixel_height.saturating_sub(
                y_offset as usize + border.bottom.get() + bottom_bar_height as usize,
            ),
            self.min_scroll_bar_height() as usize,
        );
        self.set_viewport(pane.pane_id(), Some(row), dims);
        context.invalidate();
        self.dragging.replace((item, start_event));
    }

    fn drag_ui_item(
        &mut self,
        item: UIItem,
        start_event: MouseEvent,
        x: usize,
        y: i64,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match item.item_type {
            UIItemType::Split(split) => {
                self.drag_split(item, split, start_event, x, y, context);
            }
            UIItemType::ScrollThumb => {
                self.drag_scroll_thumb(item, start_event, event, context);
            }
            _ => {
                log::error!("drag not implemented for {:?}", item);
            }
        }
    }

    fn mouse_event_ui_item(
        &mut self,
        item: UIItem,
        pane: Arc<dyn Pane>,
        _y: i64,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        self.last_ui_item.replace(item.clone());
        match item.item_type {
            UIItemType::TabBar(item) => {
                self.mouse_event_tab_bar(item, event, context);
            }
            UIItemType::AboveScrollThumb => {
                self.mouse_event_above_scroll_thumb(item, pane, event, context);
            }
            UIItemType::ScrollThumb => {
                self.mouse_event_scroll_thumb(item, pane, event, context);
            }
            UIItemType::BelowScrollThumb => {
                self.mouse_event_below_scroll_thumb(item, pane, event, context);
            }
            UIItemType::Split(split) => {
                self.mouse_event_split(item, split, event, context);
            }
            UIItemType::CloseTab(idx) => {
                self.mouse_event_close_tab(idx, event, context);
            }
            UIItemType::PaneDragHandle(pane_id) => {
                self.mouse_event_pane_drag_handle(pane_id, item, event, context);
            }
        }
    }

    pub fn mouse_event_close_tab(
        &mut self,
        idx: usize,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match event.kind {
            WMEK::Press(MousePress::Left) => {
                log::debug!("Should close tab {}", idx);
                self.close_specific_tab(idx, true);
            }
            _ => {}
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    fn do_new_tab_button_click(&mut self, button: MousePress) {
        let pane = match self.get_active_pane_or_overlay() {
            Some(pane) => pane,
            None => return,
        };
        let action = match button {
            MousePress::Left => Some(KeyAssignment::SpawnTab(SpawnTabDomain::CurrentPaneDomain)),
            MousePress::Right => Some(KeyAssignment::ShowLauncher),
            MousePress::Middle => None,
        };

        async fn dispatch_new_tab_button(
            lua: Option<Rc<mlua::Lua>>,
            window: GuiWin,
            pane: MuxPane,
            button: MousePress,
            action: Option<KeyAssignment>,
        ) -> anyhow::Result<()> {
            let default_action = match lua {
                Some(lua) => {
                    let args = lua.pack_multi((
                        window.clone(),
                        pane,
                        format!("{button:?}"),
                        action.clone(),
                    ))?;
                    config::lua::emit_event(&lua, ("new-tab-button-click".to_string(), args))
                        .await
                        .map_err(|e| {
                            log::error!("while processing new-tab-button-click event: {:#}", e);
                            e
                        })?
                }
                None => true,
            };
            if let (true, Some(assignment)) = (default_action, action) {
                window.window.notify(TermWindowNotif::PerformAssignment {
                    pane_id: pane.0,
                    assignment,
                    tx: None,
                });
            }
            Ok(())
        }
        let window = GuiWin::new(self);
        let pane = MuxPane(pane.pane_id());
        promise::spawn::spawn(config::with_lua_config_on_main_thread(move |lua| {
            dispatch_new_tab_button(lua, window, pane, button, action)
        }))
        .detach();
    }

    pub fn mouse_event_tab_bar(
        &mut self,
        item: TabBarItem,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        match event.kind {
            WMEK::Press(MousePress::Left) => match item {
                TabBarItem::Tab { tab_idx, .. } => {
                    if self.activate_tab(tab_idx as isize).is_ok() {
                        let dragged_item = self.resolve_ui_item(&event);
                        let grab_offset = dragged_item
                            .as_ref()
                            .map(|item| {
                                Point::new(
                                    event.coords.x.saturating_sub(item.x as isize),
                                    event.coords.y.saturating_sub(item.y as isize),
                                )
                            })
                            .unwrap_or_else(|| Point::new(0, 0));
                        let dragged_size = dragged_item
                            .map(|item| Size::new(item.width as isize, item.height as isize))
                            .unwrap_or_else(|| Size::new(0, 0));
                        let tab_and_target =
                            Mux::get()
                                .get_window(self.mux_window_id)
                                .and_then(|window| {
                                    let tab_id = window.get_by_idx(tab_idx)?.tab_id();
                                    let target_tab_id = window
                                        .get_last_active_idx()
                                        .and_then(|idx| window.get_by_idx(idx))
                                        .map(|tab| tab.tab_id())
                                        .filter(|target_id| *target_id != tab_id)
                                        .or_else(|| {
                                            window
                                                .iter()
                                                .find(|tab| tab.tab_id() != tab_id)
                                                .map(|tab| tab.tab_id())
                                        });
                                    Some((tab_id, target_tab_id))
                                });
                        if let Some((tab_id, same_window_target_tab_id)) = tab_and_target {
                            self.tab_drag.replace(TabDragState {
                                tab_id,
                                start_event: event.clone(),
                                grab_offset,
                                dragged_size,
                                current_coords: event.coords,
                                preview_window: None,
                                same_window_target_tab_id,
                                started: false,
                            });
                        }
                    }
                }
                TabBarItem::NewTabButton { .. } => {
                    self.do_new_tab_button_click(MousePress::Left);
                }
                TabBarItem::None | TabBarItem::LeftStatus | TabBarItem::RightStatus => {
                    let maximized = self
                        .window_state
                        .intersects(WindowState::MAXIMIZED | WindowState::FULL_SCREEN);
                    if let Some(ref window) = self.window {
                        if self.config.window_decorations
                            == WindowDecorations::INTEGRATED_BUTTONS | WindowDecorations::RESIZE
                        {
                            if self.last_mouse_click.as_ref().map(|c| c.streak) == Some(2) {
                                if maximized {
                                    window.restore();
                                } else {
                                    window.maximize();
                                }
                            }
                        }
                    }
                    // Potentially starting a drag by the tab bar
                    if !maximized {
                        self.window_drag_position.replace(event.clone());
                    }
                    context.request_drag_move();
                }
                TabBarItem::WindowButton(button) => {
                    use window::IntegratedTitleButton as Button;
                    if let Some(ref window) = self.window {
                        match button {
                            Button::Hide => window.hide(),
                            Button::Maximize => {
                                let maximized = self
                                    .window_state
                                    .intersects(WindowState::MAXIMIZED | WindowState::FULL_SCREEN);
                                if maximized {
                                    window.restore();
                                } else {
                                    window.maximize();
                                }
                            }
                            Button::Close => self.close_requested(&window.clone()),
                        }
                    }
                }
            },
            WMEK::Press(MousePress::Middle) => match item {
                TabBarItem::Tab { tab_idx, .. } => {
                    self.close_specific_tab(tab_idx, true);
                }
                TabBarItem::NewTabButton { .. } => {
                    self.do_new_tab_button_click(MousePress::Middle);
                }
                TabBarItem::None
                | TabBarItem::LeftStatus
                | TabBarItem::RightStatus
                | TabBarItem::WindowButton(_) => {}
            },
            WMEK::Press(MousePress::Right) => match item {
                TabBarItem::Tab { .. } => {
                    self.show_tab_navigator();
                }
                TabBarItem::NewTabButton { .. } => {
                    self.do_new_tab_button_click(MousePress::Right);
                }
                TabBarItem::None
                | TabBarItem::LeftStatus
                | TabBarItem::RightStatus
                | TabBarItem::WindowButton(_) => {}
            },
            WMEK::Move => match item {
                TabBarItem::None | TabBarItem::LeftStatus | TabBarItem::RightStatus => {
                    context.set_window_drag_position(event.screen_coords);
                }
                TabBarItem::WindowButton(window::IntegratedTitleButton::Maximize) => {
                    let item = self.last_ui_item.clone().unwrap();
                    let bounds: ::window::ScreenRect = euclid::rect(
                        item.x as isize - (event.coords.x as isize - event.screen_coords.x),
                        item.y as isize - (event.coords.y as isize - event.screen_coords.y),
                        item.width as isize,
                        item.height as isize,
                    );
                    context.set_maximize_button_position(bounds);
                }
                TabBarItem::WindowButton(_)
                | TabBarItem::Tab { .. }
                | TabBarItem::NewTabButton { .. } => {}
            },
            WMEK::VertWheel(n) => {
                if self.config.mouse_wheel_scrolls_tabs {
                    self.activate_tab_relative(if n < 1 { 1 } else { -1 }, true)
                        .ok();
                }
            }
            _ => {}
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_above_scroll_thumb(
        &mut self,
        _item: UIItem,
        pane: Arc<dyn Pane>,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if let WMEK::Press(MousePress::Left) = event.kind {
            let dims = pane.get_dimensions();
            let current_viewport = self.get_viewport(pane.pane_id());
            // Page up
            self.set_viewport(
                pane.pane_id(),
                Some(
                    current_viewport
                        .unwrap_or(dims.physical_top)
                        .saturating_sub(self.terminal_size.rows.try_into().unwrap()),
                ),
                dims,
            );
            context.invalidate();
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_below_scroll_thumb(
        &mut self,
        _item: UIItem,
        pane: Arc<dyn Pane>,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if let WMEK::Press(MousePress::Left) = event.kind {
            let dims = pane.get_dimensions();
            let current_viewport = self.get_viewport(pane.pane_id());
            // Page down
            self.set_viewport(
                pane.pane_id(),
                Some(
                    current_viewport
                        .unwrap_or(dims.physical_top)
                        .saturating_add(self.terminal_size.rows.try_into().unwrap()),
                ),
                dims,
            );
            context.invalidate();
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_scroll_thumb(
        &mut self,
        item: UIItem,
        _pane: Arc<dyn Pane>,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        if let WMEK::Press(MousePress::Left) = event.kind {
            // Start a scroll drag
            // self.scroll_drag_start = Some(from_top);
            self.dragging = Some((item, event));
        }
        context.set_cursor(Some(MouseCursor::Arrow));
    }

    pub fn mouse_event_split(
        &mut self,
        item: UIItem,
        split: PositionedSplit,
        event: MouseEvent,
        context: &dyn WindowOps,
    ) {
        context.set_cursor(Some(match &split.direction {
            SplitDirection::Horizontal => MouseCursor::SizeLeftRight,
            SplitDirection::Vertical => MouseCursor::SizeUpDown,
        }));

        if event.kind == WMEK::Press(MousePress::Left) {
            self.dragging.replace((item, event));
        }
    }

    fn mouse_event_terminal(
        &mut self,
        mut pane: Arc<dyn Pane>,
        position: ClickPosition,
        event: MouseEvent,
        context: &dyn WindowOps,
        capture_mouse: bool,
    ) {
        let mut is_click_to_focus_pane = false;

        let ClickPosition {
            mut column,
            mut row,
            mut x_pixel_offset,
            mut y_pixel_offset,
        } = position;

        let is_already_captured = matches!(
            self.current_mouse_capture,
            Some(MouseCapture::TerminalPane(_))
        );

        for pos in self.get_panes_to_render() {
            if !is_already_captured
                && row >= pos.top as i64
                && row <= (pos.top + pos.height) as i64
                && column >= pos.left
                && column <= pos.left + pos.width
            {
                if pane.pane_id() != pos.pane.pane_id() {
                    // We're over a pane that isn't active
                    match &event.kind {
                        WMEK::Press(_) => {
                            let mux = Mux::get();
                            mux.get_active_tab_for_window(self.mux_window_id)
                                .map(|tab| tab.set_active_idx(pos.index));

                            pane = Arc::clone(&pos.pane);
                            is_click_to_focus_pane = true;
                        }
                        WMEK::Move => {
                            if self.config.pane_focus_follows_mouse {
                                let mux = Mux::get();
                                mux.get_active_tab_for_window(self.mux_window_id)
                                    .map(|tab| tab.set_active_idx(pos.index));

                                pane = Arc::clone(&pos.pane);
                                context.invalidate();
                            }
                        }
                        WMEK::Release(_) | WMEK::HorzWheel(_) => {}
                        WMEK::VertWheel(_) => {
                            // Let wheel events route to the hovered pane,
                            // even if it doesn't have focus
                            pane = Arc::clone(&pos.pane);
                            context.invalidate();
                        }
                    }
                }
                column = column.saturating_sub(pos.left);
                row = row.saturating_sub(pos.top as i64);
                break;
            } else if is_already_captured && pane.pane_id() == pos.pane.pane_id() {
                column = column.saturating_sub(pos.left);
                row = row.saturating_sub(pos.top as i64).max(0);

                if position.column < pos.left {
                    x_pixel_offset -= self.render_metrics.cell_size.width
                        * (pos.left as isize - position.column as isize);
                }
                if position.row < pos.top as i64 {
                    y_pixel_offset -= self.render_metrics.cell_size.height
                        * (pos.top as isize - position.row as isize);
                }

                break;
            }
        }

        if capture_mouse {
            self.current_mouse_capture = Some(MouseCapture::TerminalPane(pane.pane_id()));
        }

        let is_focused = if let Some(focused) = self.focused.as_ref() {
            !self.config.swallow_mouse_click_on_window_focus
                || (focused.elapsed() > Duration::from_millis(200))
        } else {
            false
        };

        if self.focused.is_some() && !is_focused {
            if matches!(&event.kind, WMEK::Press(_))
                && self.config.swallow_mouse_click_on_window_focus
            {
                // Entering click to focus state
                self.is_click_to_focus_window = true;
                context.invalidate();
                log::trace!("enter click to focus");
                return;
            }
        }
        if self.is_click_to_focus_window && matches!(&event.kind, WMEK::Release(_)) {
            // Exiting click to focus state
            self.is_click_to_focus_window = false;
            context.invalidate();
            log::trace!("exit click to focus");
            return;
        }

        let allow_action = if self.is_click_to_focus_window || !is_focused {
            matches!(&event.kind, WMEK::VertWheel(_) | WMEK::HorzWheel(_))
        } else {
            true
        };

        log::trace!(
            "is_focused={} allow_action={} event={:?}",
            is_focused,
            allow_action,
            event
        );

        let dims = pane.get_dimensions();
        let stable_row = self
            .get_viewport(pane.pane_id())
            .unwrap_or(dims.physical_top)
            + row as StableRowIndex;

        self.pane_state(pane.pane_id())
            .mouse_terminal_coords
            .replace((
                ClickPosition {
                    column,
                    row,
                    x_pixel_offset,
                    y_pixel_offset,
                },
                stable_row,
            ));

        pane.apply_hyperlinks(stable_row..stable_row + 1, &self.config.hyperlink_rules);

        struct FindCurrentLink {
            current: Option<Arc<Hyperlink>>,
            stable_row: StableRowIndex,
            column: usize,
        }

        impl WithPaneLines for FindCurrentLink {
            fn with_lines_mut(&mut self, stable_top: StableRowIndex, lines: &mut [&mut Line]) {
                if stable_top == self.stable_row {
                    if let Some(line) = lines.get(0) {
                        if let Some(cell) = line.get_cell(self.column) {
                            self.current = cell.attrs().hyperlink().cloned();
                        }
                    }
                }
            }
        }

        let mut find_link = FindCurrentLink {
            current: None,
            stable_row,
            column,
        };
        pane.with_lines_mut(stable_row..stable_row + 1, &mut find_link);
        let new_highlight = find_link.current;

        match (self.current_highlight.as_ref(), new_highlight) {
            (Some(old_link), Some(new_link)) if Arc::ptr_eq(&old_link, &new_link) => {
                // Unchanged
            }
            (None, None) => {
                // Unchanged
            }
            (_, rhs) => {
                // We're hovering over a different URL, so invalidate and repaint
                // so that we render the underline correctly
                self.current_highlight = rhs;
                context.invalidate();
            }
        };

        let outside_window = event.coords.x < 0
            || event.coords.x as usize > self.dimensions.pixel_width
            || event.coords.y < 0
            || event.coords.y as usize > self.dimensions.pixel_height;

        context.set_cursor(Some(if self.current_highlight.is_some() {
            // When hovering over a hyperlink, show an appropriate
            // mouse cursor to give the cue that it is clickable
            MouseCursor::Hand
        } else if pane.is_mouse_grabbed() || outside_window {
            MouseCursor::Arrow
        } else {
            MouseCursor::Text
        }));

        let event_trigger_type = match &event.kind {
            WMEK::Press(press) => {
                let press = mouse_press_to_tmb(press);
                match self.last_mouse_click.as_ref() {
                    Some(LastMouseClick { streak, button, .. }) if *button == press => {
                        Some(MouseEventTrigger::Down {
                            streak: *streak,
                            button: press,
                        })
                    }
                    _ => None,
                }
            }
            WMEK::Release(press) => {
                let press = mouse_press_to_tmb(press);
                match self.last_mouse_click.as_ref() {
                    Some(LastMouseClick { streak, button, .. }) if *button == press => {
                        Some(MouseEventTrigger::Up {
                            streak: *streak,
                            button: press,
                        })
                    }
                    _ => None,
                }
            }
            WMEK::Move => {
                if !self.current_mouse_buttons.is_empty() {
                    if let Some(LastMouseClick { streak, button, .. }) =
                        self.last_mouse_click.as_ref()
                    {
                        if Some(*button)
                            == self.current_mouse_buttons.last().map(mouse_press_to_tmb)
                        {
                            Some(MouseEventTrigger::Drag {
                                streak: *streak,
                                button: *button,
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            WMEK::VertWheel(amount) => Some(match *amount {
                0 => return,
                1.. => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelUp(*amount as usize),
                },
                _ => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelDown(-amount as usize),
                },
            }),
            WMEK::HorzWheel(amount) => Some(match *amount {
                0 => return,
                1.. => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelLeft(*amount as usize),
                },
                _ => MouseEventTrigger::Down {
                    streak: 1,
                    button: MouseButton::WheelRight(-amount as usize),
                },
            }),
        };

        if allow_action {
            if let Some(mut event_trigger_type) = event_trigger_type {
                self.current_event = Some(event_trigger_type.to_dynamic());
                let mut modifiers = event.modifiers;

                // Since we use shift to force assessing the mouse bindings, pretend
                // that shift is not one of the mods when the mouse is grabbed.
                let mut mouse_reporting = pane.is_mouse_grabbed();
                if mouse_reporting {
                    if modifiers.contains(self.config.bypass_mouse_reporting_modifiers) {
                        modifiers.remove(self.config.bypass_mouse_reporting_modifiers);
                        mouse_reporting = false;
                    }
                }

                if mouse_reporting {
                    // If they were scrolled back prior to launching an
                    // application that captures the mouse, then mouse based
                    // scrolling assignments won't have any effect.
                    // Ensure that we scroll to the bottom if they try to
                    // use the mouse so that things are less surprising
                    self.scroll_to_bottom(&pane);
                }

                // normalize delta and streak to make mouse assignment
                // easier to wrangle
                match event_trigger_type {
                    MouseEventTrigger::Down {
                        ref mut streak,
                        button:
                            MouseButton::WheelUp(ref mut delta)
                            | MouseButton::WheelDown(ref mut delta)
                            | MouseButton::WheelLeft(ref mut delta)
                            | MouseButton::WheelRight(ref mut delta),
                    }
                    | MouseEventTrigger::Up {
                        ref mut streak,
                        button:
                            MouseButton::WheelUp(ref mut delta)
                            | MouseButton::WheelDown(ref mut delta)
                            | MouseButton::WheelLeft(ref mut delta)
                            | MouseButton::WheelRight(ref mut delta),
                    }
                    | MouseEventTrigger::Drag {
                        ref mut streak,
                        button:
                            MouseButton::WheelUp(ref mut delta)
                            | MouseButton::WheelDown(ref mut delta)
                            | MouseButton::WheelLeft(ref mut delta)
                            | MouseButton::WheelRight(ref mut delta),
                    } => {
                        *streak = 1;
                        *delta = 1;
                    }
                    _ => {}
                };

                let mouse_mods = config::MouseEventTriggerMods {
                    mods: modifiers,
                    mouse_reporting,
                    alt_screen: if pane.is_alt_screen_active() {
                        MouseEventAltScreen::True
                    } else {
                        MouseEventAltScreen::False
                    },
                };

                if let Some(action) = self.input_map.lookup_mouse(event_trigger_type, mouse_mods) {
                    self.perform_key_assignment(&pane, &action).ok();
                    return;
                }
            }
        }

        let mouse_event = wezterm_term::MouseEvent {
            kind: match event.kind {
                WMEK::Move => TMEK::Move,
                WMEK::VertWheel(_) | WMEK::HorzWheel(_) | WMEK::Press(_) => TMEK::Press,
                WMEK::Release(_) => TMEK::Release,
            },
            button: match event.kind {
                WMEK::Release(ref press) | WMEK::Press(ref press) => mouse_press_to_tmb(press),
                WMEK::Move => {
                    if event.mouse_buttons == WMB::LEFT {
                        TMB::Left
                    } else if event.mouse_buttons == WMB::RIGHT {
                        TMB::Right
                    } else if event.mouse_buttons == WMB::MIDDLE {
                        TMB::Middle
                    } else {
                        TMB::None
                    }
                }
                WMEK::VertWheel(amount) => {
                    if amount > 0 {
                        TMB::WheelUp(amount as usize)
                    } else {
                        TMB::WheelDown((-amount) as usize)
                    }
                }
                WMEK::HorzWheel(amount) => {
                    if amount > 0 {
                        TMB::WheelLeft(amount as usize)
                    } else {
                        TMB::WheelRight((-amount) as usize)
                    }
                }
            },
            x: column,
            y: row,
            x_pixel_offset,
            y_pixel_offset,
            modifiers: event.modifiers,
        };

        if allow_action
            && !(self.config.swallow_mouse_click_on_pane_focus && is_click_to_focus_pane)
        {
            pane.mouse_event(mouse_event).ok();
        }

        match event.kind {
            WMEK::Move => {}
            _ => {
                context.invalidate();
            }
        }
    }
}

fn mouse_press_to_tmb(press: &MousePress) -> TMB {
    match press {
        MousePress::Left => TMB::Left,
        MousePress::Right => TMB::Right,
        MousePress::Middle => TMB::Middle,
    }
}
