use crate::quad::TripleLayerQuadAllocator;
use crate::termwindow::render::RenderScreenLineParams;
use crate::utilsprites::RenderMetrics;
use config::ConfigHandle;
use mux::renderable::RenderableDimensions;
use wezterm_term::color::ColorAttribute;
use window::color::LinearRgba;

const DRAG_GHOST_ALPHA: f32 = 0.55;
const DROP_PREVIEW_ALPHA: f32 = 0.28;
const DROP_BORDER_ALPHA: f32 = 0.9;
const DROP_BORDER_WIDTH_PIXELS: f32 = 3.0;

impl crate::TermWindow {
    pub fn paint_tab_bar(&mut self, layers: &mut TripleLayerQuadAllocator) -> anyhow::Result<()> {
        if self.config.use_fancy_tab_bar {
            if self.fancy_tab_bar.is_none() {
                let palette = self.palette().clone();
                let tab_bar = self.build_fancy_tab_bar(&palette)?;
                self.fancy_tab_bar.replace(tab_bar);
            }

            self.ui_items.append(&mut self.paint_fancy_tab_bar()?);
            return Ok(());
        }

        let border = self.get_os_border();

        let palette = self.palette().clone();
        let tab_bar_height = self.tab_bar_pixel_height()?;
        let tab_bar_y = if self.config.tab_bar_at_bottom {
            ((self.dimensions.pixel_height as f32) - (tab_bar_height + border.bottom.get() as f32))
                .max(0.)
        } else {
            border.top.get() as f32
        };

        // Register the tab bar location
        self.ui_items.append(&mut self.tab_bar.compute_ui_items(
            tab_bar_y as usize,
            self.render_metrics.cell_size.height as usize,
            self.render_metrics.cell_size.width as usize,
        ));

        let window_is_transparent =
            !self.window_background.is_empty() || self.config.window_background_opacity != 1.0;
        let gl_state = self.render_state.as_ref().unwrap();
        let white_space = gl_state.util_sprites.white_space.texture_coords();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();
        let default_bg = palette
            .resolve_bg(ColorAttribute::Default)
            .to_linear()
            .mul_alpha(if window_is_transparent {
                0.
            } else {
                self.config.text_background_opacity
            });

        self.render_screen_line(
            RenderScreenLineParams {
                top_pixel_y: tab_bar_y,
                left_pixel_x: 0.,
                pixel_width: self.dimensions.pixel_width as f32,
                stable_line_idx: None,
                line: self.tab_bar.line(),
                selection: 0..0,
                cursor: &Default::default(),
                palette: &palette,
                dims: &RenderableDimensions {
                    cols: self.dimensions.pixel_width
                        / self.render_metrics.cell_size.width as usize,
                    physical_top: 0,
                    scrollback_rows: 0,
                    scrollback_top: 0,
                    viewport_rows: 1,
                    dpi: self.terminal_size.dpi,
                    pixel_height: self.render_metrics.cell_size.height as usize,
                    pixel_width: self.terminal_size.pixel_width,
                    reverse_video: false,
                },
                config: &self.config,
                cursor_border_color: LinearRgba::default(),
                foreground: palette.foreground.to_linear(),
                pane: None,
                is_active: true,
                selection_fg: LinearRgba::default(),
                selection_bg: LinearRgba::default(),
                cursor_fg: LinearRgba::default(),
                cursor_bg: LinearRgba::default(),
                cursor_is_default_color: true,
                white_space,
                filled_box,
                window_is_transparent,
                default_bg,
                style: None,
                font: None,
                use_pixel_positioning: self.config.experimental_pixel_positioning,
                render_metrics: self.render_metrics,
                shape_key: None,
                password_input: false,
            },
            layers,
        )?;

        Ok(())
    }

    pub fn paint_drag_effects(
        &mut self,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let color = self.palette().selection_bg.to_linear();

        if let Some(preview) = self.drag_drop_preview {
            self.filled_rectangle(layers, 2, preview.rect, color.mul_alpha(DROP_PREVIEW_ALPHA))?;

            let rect = preview.rect;
            let border = DROP_BORDER_WIDTH_PIXELS
                .min(rect.width() / 2.0)
                .min(rect.height() / 2.0);
            let border_color = color.mul_alpha(DROP_BORDER_ALPHA);
            for edge in [
                euclid::rect(rect.min_x(), rect.min_y(), rect.width(), border),
                euclid::rect(rect.min_x(), rect.max_y() - border, rect.width(), border),
                euclid::rect(rect.min_x(), rect.min_y(), border, rect.height()),
                euclid::rect(rect.max_x() - border, rect.min_y(), border, rect.height()),
            ] {
                self.filled_rectangle(layers, 2, edge, border_color)?;
            }
        }

        let drag_geometry = self
            .tab_drag
            .as_ref()
            .filter(|drag| drag.started)
            .map(|drag| (drag.dragged_size, drag.current_coords, drag.grab_offset))
            .or_else(|| {
                self.pane_drag
                    .as_ref()
                    .filter(|drag| drag.started)
                    .map(|drag| (drag.dragged_size, drag.current_coords, drag.grab_offset))
            });
        if let Some((dragged_size, current_coords, grab_offset)) = drag_geometry {
            let width = dragged_size.width.max(0) as f32;
            let height = dragged_size.height.max(0) as f32;
            if width > 0.0 && height > 0.0 {
                let ghost = euclid::rect(
                    (current_coords.x - grab_offset.x) as f32,
                    (current_coords.y - grab_offset.y) as f32,
                    width,
                    height,
                );
                self.filled_rectangle(layers, 2, ghost, color.mul_alpha(DRAG_GHOST_ALPHA))?;
            }
        }

        Ok(())
    }

    pub fn tab_bar_pixel_height_impl(
        config: &ConfigHandle,
        fontconfig: &wezterm_font::FontConfiguration,
        render_metrics: &RenderMetrics,
    ) -> anyhow::Result<f32> {
        if config.use_fancy_tab_bar {
            let font = fontconfig.title_font()?;
            Ok((font.metrics().cell_height.get() as f32 * 1.75).ceil())
        } else {
            Ok(render_metrics.cell_size.height as f32)
        }
    }

    pub fn tab_bar_pixel_height(&self) -> anyhow::Result<f32> {
        Self::tab_bar_pixel_height_impl(&self.config, &self.fonts, &self.render_metrics)
    }
}
