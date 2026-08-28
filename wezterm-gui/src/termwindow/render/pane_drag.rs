use crate::quad::TripleLayerQuadAllocator;
use crate::termwindow::{UIItem, UIItemType};
use mux::tab::PositionedPane;
use window::RectF;

const PANE_DRAG_HANDLE_WIDTH_PIXELS: f32 = 24.0;
const PANE_DRAG_HANDLE_HEIGHT_PIXELS: f32 = 18.0;
const PANE_DRAG_HANDLE_INSET_PIXELS: f32 = 6.0;
const PANE_DRAG_HANDLE_IDLE_ALPHA: f32 = 0.18;
const PANE_DRAG_HANDLE_HOVER_ALPHA: f32 = 0.68;
const PANE_DRAG_HANDLE_DOT_ALPHA: f32 = 0.9;
const PANE_DRAG_HANDLE_DOT_SIZE_PIXELS: f32 = 2.0;
const PANE_DRAG_HANDLE_DOT_GAP_PIXELS: f32 = 4.0;

impl crate::TermWindow {
    pub(crate) fn pane_drag_bounds(&self, positioned: &PositionedPane) -> RectF {
        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;
        let (padding_left, padding_top) = self.padding_left_top();
        let tab_bar_height = if self.show_tab_bar && !self.config.tab_bar_at_bottom {
            self.tab_bar_pixel_height().unwrap_or(0.0)
        } else {
            0.0
        };
        let border = self.get_os_border();

        euclid::rect(
            padding_left + border.left.get() as f32 - cell_width / 2.0
                + positioned.left as f32 * cell_width,
            tab_bar_height + padding_top + border.top.get() as f32 - cell_height / 2.0
                + positioned.top as f32 * cell_height,
            positioned.width as f32 * cell_width,
            positioned.height as f32 * cell_height,
        )
    }

    pub fn paint_pane_drag_handles(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        if self.tab_drag.is_some() {
            return Ok(());
        }

        let palette = self.palette().clone();
        let color = palette.selection_bg.to_linear();
        let dot_base_color = palette.foreground.to_linear();
        let Some(active_tab) = mux::Mux::get().get_active_tab_for_window(self.mux_window_id) else {
            return Ok(());
        };
        for positioned in active_tab.iter_panes() {
            let pane_id = positioned.pane.pane_id();
            let pane_bounds = self.pane_drag_bounds(&positioned);
            let width = PANE_DRAG_HANDLE_WIDTH_PIXELS.min(pane_bounds.width().max(0.0));
            let height = PANE_DRAG_HANDLE_HEIGHT_PIXELS.min(pane_bounds.height().max(0.0));
            if width <= 0.0 || height <= 0.0 {
                continue;
            }

            let handle = euclid::rect(
                (pane_bounds.max_x() - width - PANE_DRAG_HANDLE_INSET_PIXELS)
                    .max(pane_bounds.min_x()),
                (pane_bounds.min_y() + PANE_DRAG_HANDLE_INSET_PIXELS)
                    .min(pane_bounds.max_y() - height),
                width,
                height,
            );
            self.ui_items.push(UIItem {
                x: handle.min_x().max(0.0) as usize,
                y: handle.min_y().max(0.0) as usize,
                width: handle.width().ceil() as usize,
                height: handle.height().ceil() as usize,
                item_type: UIItemType::PaneDragHandle(pane_id),
            });

            let hovered = self
                .last_ui_item
                .as_ref()
                .map(|item| item.item_type == UIItemType::PaneDragHandle(pane_id))
                .unwrap_or(false);
            let dragging = self
                .pane_drag
                .as_ref()
                .map(|drag| drag.pane_id == pane_id)
                .unwrap_or(false);
            let alpha = if hovered || dragging {
                PANE_DRAG_HANDLE_HOVER_ALPHA
            } else if positioned.is_active {
                PANE_DRAG_HANDLE_IDLE_ALPHA
            } else {
                continue;
            };

            self.filled_rectangle(layers, 2, handle, color.mul_alpha(alpha))?;

            let dots_width = PANE_DRAG_HANDLE_DOT_SIZE_PIXELS + PANE_DRAG_HANDLE_DOT_GAP_PIXELS;
            let dots_height =
                PANE_DRAG_HANDLE_DOT_SIZE_PIXELS + PANE_DRAG_HANDLE_DOT_GAP_PIXELS * 2.0;
            let dots_left = handle.center().x - dots_width / 2.0;
            let dots_top = handle.center().y - dots_height / 2.0;
            let dot_color = dot_base_color.mul_alpha(alpha * PANE_DRAG_HANDLE_DOT_ALPHA);
            for column in 0..2 {
                for row in 0..3 {
                    self.filled_rectangle(
                        layers,
                        2,
                        euclid::rect(
                            dots_left + column as f32 * PANE_DRAG_HANDLE_DOT_GAP_PIXELS,
                            dots_top + row as f32 * PANE_DRAG_HANDLE_DOT_GAP_PIXELS,
                            PANE_DRAG_HANDLE_DOT_SIZE_PIXELS,
                            PANE_DRAG_HANDLE_DOT_SIZE_PIXELS,
                        ),
                        dot_color,
                    )?;
                }
            }
        }
        Ok(())
    }
}
