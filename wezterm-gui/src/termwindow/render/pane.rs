use crate::quad::{
    HeapQuadAllocator, QuadTrait, TripleLayerQuadAllocator, TripleLayerQuadAllocatorTrait,
};
use crate::selection::SelectionRange;
use crate::termwindow::box_model::*;
use crate::termwindow::render::{
    same_hyperlink, CursorProperties, LineQuadCacheKey, LineQuadCacheValue, LineToEleShapeCacheKey,
    RenderScreenLineParams,
};
use crate::termwindow::prevcursor::CursorPixelRect;
use crate::termwindow::{ScrollHit, UIItem, UIItemType};
use ::window::bitmaps::TextureRect;
use ::window::DeadKeyStatus;
use anyhow::Context;
use config::VisualBellTarget;
use mux::pane::{PaneId, WithPaneLines};
use mux::renderable::{RenderableDimensions, StableCursorPosition};
use mux::tab::PositionedPane;
use termwiz::surface::CursorShape;
use ordered_float::NotNan;
use std::time::{Duration, Instant};
use wezterm_dynamic::Value;
use wezterm_term::color::{ColorAttribute, ColorPalette};
use wezterm_term::{Line, StableRowIndex};
use window::color::LinearRgba;

/// Inputs for the dedicated cursor trail pass, gathered from paint_pane.
///
/// The smear pass owns the entire cursor when enabled (see ADR 0001): it draws
/// the cursor's shape (block/bar/underline) and its resting colour as well as
/// the trail, so the trail is the cursor stretched rather than a separate block.
struct CursorSmearParams<'a> {
    cursor: &'a StableCursorPosition,
    viewport_top: StableRowIndex,
    /// Window-absolute pixel coordinates of this pane's origin (top-left corner),
    /// i.e. already offset by the pane's position within the window (pos.left /
    /// pos.top). `cursor_target_rect` adds only the in-pane cell offset on top, so
    /// both axes must carry the pane offset here symmetrically — otherwise a pane
    /// that is not at the window origin (e.g. the lower pane of a horizontal
    /// split) draws its cursor at the wrong place.
    left_pixel_x: f32,
    top_pixel_y: f32,
    cell_width: f32,
    cell_height: f32,
    /// Effective cursor shape (config default resolved against the cursor's own
    /// shape) used to narrow the trail rect into a bar/underline.
    shape: CursorShape,
    /// True when the per-line pass renders a special cursor that the smear must
    /// not take over: an active IME pre-edit (variable-width composition block)
    /// or password input (lock glyph). The smear stands down and snaps without a
    /// trail so it doesn't paint a second cursor over the special one.
    defer_to_per_line: bool,
    /// Resolved cursor colour for the *target* cell, matching what
    /// compute_cell_fg_bg would pick for a plain (non-IME, non-selection)
    /// cursor — including force_reverse_video_cursor. Used for the whole trail.
    cursor_color: LinearRgba,
}

impl crate::TermWindow {
    fn paint_pane_box_model(&mut self, pos: &PositionedPane) -> anyhow::Result<()> {
        let computed = self.build_pane(pos)?;
        let mut ui_items = computed.ui_items();
        self.ui_items.append(&mut ui_items);
        let gl_state = self.render_state.as_ref().unwrap();
        self.render_element(&computed, gl_state, None)
    }

    pub fn paint_pane(
        &mut self,
        pos: &PositionedPane,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        if self.config.use_box_model_render {
            return self.paint_pane_box_model(pos);
        }

        self.check_for_dirty_lines_and_invalidate_selection(&pos.pane);
        /*
        let zone = {
            let dims = pos.pane.get_dimensions();
            let position = self
                .get_viewport(pos.pane.pane_id())
                .unwrap_or(dims.physical_top);

            let zones = self.get_semantic_zones(&pos.pane);
            let idx = match zones.binary_search_by(|zone| zone.start_y.cmp(&position)) {
                Ok(idx) | Err(idx) => idx,
            };
            let idx = ((idx as isize) - 1).max(0) as usize;
            zones.get(idx).cloned()
        };
        */

        let global_cursor_fg = self.palette().cursor_fg;
        let global_cursor_bg = self.palette().cursor_bg;
        let config = self.config.clone();
        let palette = pos.pane.palette();

        let (padding_left, padding_top) = self.padding_left_top();

        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height()
                .context("tab_bar_pixel_height")?
        } else {
            0.
        };
        let (top_bar_height, bottom_bar_height) = if self.config.tab_bar_at_bottom {
            (0.0, tab_bar_height)
        } else {
            (tab_bar_height, 0.0)
        };

        let border = self.get_os_border();
        let top_pixel_y = top_bar_height + padding_top + border.top.get() as f32;

        let cursor = pos.pane.get_cursor_position();
        if pos.is_active {
            self.prev_cursor.update(&cursor);
        }

        let pane_id = pos.pane.pane_id();
        let current_viewport = self.get_viewport(pane_id);
        let dims = pos.pane.get_dimensions();

        let gl_state = self.render_state.as_ref().unwrap();

        let cursor_border_color = palette.cursor_border.to_linear();
        let foreground = palette.foreground.to_linear();
        let white_space = gl_state.util_sprites.white_space.texture_coords();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();

        let window_is_transparent =
            !self.window_background.is_empty() || config.window_background_opacity != 1.0;

        let default_bg = palette
            .resolve_bg(ColorAttribute::Default)
            .to_linear()
            .mul_alpha(if window_is_transparent {
                0.
            } else {
                config.text_background_opacity
            });

        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;
        let background_rect = {
            // We want to fill out to the edges of the splits
            let (x, width_delta) = if pos.left == 0 {
                (
                    0.,
                    padding_left + border.left.get() as f32 + (cell_width / 2.0),
                )
            } else {
                (
                    padding_left + border.left.get() as f32 - (cell_width / 2.0)
                        + (pos.left as f32 * cell_width),
                    cell_width,
                )
            };

            let (y, height_delta) = if pos.top == 0 {
                (
                    (top_pixel_y - padding_top),
                    padding_top + (cell_height / 2.0),
                )
            } else {
                (
                    top_pixel_y + (pos.top as f32 * cell_height) - (cell_height / 2.0),
                    cell_height,
                )
            };
            euclid::rect(
                x,
                y,
                // Go all the way to the right edge if we're right-most
                if pos.left + pos.width >= self.terminal_size.cols as usize {
                    self.dimensions.pixel_width as f32 - x
                } else {
                    (pos.width as f32 * cell_width) + width_delta
                },
                // Go all the way to the bottom if we're bottom-most
                if pos.top + pos.height >= self.terminal_size.rows as usize {
                    self.dimensions.pixel_height as f32 - y
                } else {
                    (pos.height as f32 * cell_height) + height_delta as f32
                },
            )
        };

        if self.window_background.is_empty() {
            // Per-pane, palette-specified background

            let mut quad = self
                .filled_rectangle(
                    layers,
                    0,
                    background_rect,
                    palette
                        .background
                        .to_linear()
                        .mul_alpha(config.window_background_opacity),
                )
                .context("filled_rectangle")?;
            quad.set_hsv(if pos.is_active {
                None
            } else {
                Some(config.inactive_pane_hsb)
            });
        }

        {
            // If the bell is ringing, we draw another background layer over the
            // top of this in the configured bell color
            if let Some(intensity) = self.get_intensity_if_bell_target_ringing(
                &pos.pane,
                &config,
                VisualBellTarget::BackgroundColor,
            ) {
                // target background color
                let LinearRgba(r, g, b, _) = config
                    .resolved_palette
                    .visual_bell
                    .as_deref()
                    .unwrap_or(&palette.foreground)
                    .to_linear();

                let background = if window_is_transparent {
                    // for transparent windows, we fade in the target color
                    // by adjusting its alpha
                    LinearRgba::with_components(r, g, b, intensity)
                } else {
                    // otherwise We'll interpolate between the background color
                    // and the the target color
                    let (r1, g1, b1, a) = palette
                        .background
                        .to_linear()
                        .mul_alpha(config.window_background_opacity)
                        .tuple();
                    LinearRgba::with_components(
                        r1 + (r - r1) * intensity,
                        g1 + (g - g1) * intensity,
                        b1 + (b - b1) * intensity,
                        a,
                    )
                };
                log::trace!("bell color is {:?}", background);

                let mut quad = self
                    .filled_rectangle(layers, 0, background_rect, background)
                    .context("filled_rectangle")?;

                quad.set_hsv(if pos.is_active {
                    None
                } else {
                    Some(config.inactive_pane_hsb)
                });
            }
        }

        // TODO: we only have a single scrollbar in a single position.
        // We only update it for the active pane, but we should probably
        // do a per-pane scrollbar.  That will require more extensive
        // changes to ScrollHit, mouse positioning, PositionedPane
        // and tab size calculation.
        if pos.is_active && self.show_scroll_bar {
            let thumb_y_offset = top_bar_height as usize + border.top.get();

            let min_height = self.min_scroll_bar_height();

            let info = ScrollHit::thumb(
                &*pos.pane,
                current_viewport,
                self.dimensions.pixel_height.saturating_sub(
                    thumb_y_offset + border.bottom.get() + bottom_bar_height as usize,
                ),
                min_height as usize,
            );
            let abs_thumb_top = thumb_y_offset + info.top;
            let thumb_size = info.height;
            let color = palette.scrollbar_thumb.to_linear();

            // Adjust the scrollbar thumb position
            let config = &self.config;
            let padding = self.effective_right_padding(&config) as f32;

            let thumb_x = self.dimensions.pixel_width - padding as usize - border.right.get();

            // Register the scroll bar location
            self.ui_items.push(UIItem {
                x: thumb_x,
                width: padding as usize,
                y: thumb_y_offset,
                height: info.top,
                item_type: UIItemType::AboveScrollThumb,
            });
            self.ui_items.push(UIItem {
                x: thumb_x,
                width: padding as usize,
                y: abs_thumb_top,
                height: thumb_size,
                item_type: UIItemType::ScrollThumb,
            });
            self.ui_items.push(UIItem {
                x: thumb_x,
                width: padding as usize,
                y: abs_thumb_top + thumb_size,
                height: self
                    .dimensions
                    .pixel_height
                    .saturating_sub(abs_thumb_top + thumb_size),
                item_type: UIItemType::BelowScrollThumb,
            });

            self.filled_rectangle(
                layers,
                2,
                euclid::rect(
                    thumb_x as f32,
                    abs_thumb_top as f32,
                    padding,
                    thumb_size as f32,
                ),
                color,
            )
            .context("filled_rectangle")?;
        }

        let (selrange, rectangular) = {
            let sel = self.selection(pos.pane.pane_id());
            (sel.range.clone(), sel.rectangular)
        };

        let start = Instant::now();
        let selection_fg = palette.selection_fg.to_linear();
        let selection_bg = palette.selection_bg.to_linear();
        let cursor_fg = palette.cursor_fg.to_linear();
        let cursor_bg = palette.cursor_bg.to_linear();
        let cursor_is_default_color =
            palette.cursor_fg == global_cursor_fg && palette.cursor_bg == global_cursor_bg;

        {
            let stable_range = match current_viewport {
                Some(top) => top..top + dims.viewport_rows as StableRowIndex,
                None => dims.physical_top..dims.physical_top + dims.viewport_rows as StableRowIndex,
            };

            pos.pane
                .apply_hyperlinks(stable_range.clone(), &self.config.hyperlink_rules);

            struct LineRender<'a, 'b> {
                term_window: &'a mut crate::TermWindow,
                selrange: Option<SelectionRange>,
                rectangular: bool,
                dims: RenderableDimensions,
                top_pixel_y: f32,
                left_pixel_x: f32,
                pos: &'a PositionedPane,
                pane_id: PaneId,
                cursor: &'a StableCursorPosition,
                palette: &'a ColorPalette,
                default_bg: LinearRgba,
                cursor_border_color: LinearRgba,
                selection_fg: LinearRgba,
                selection_bg: LinearRgba,
                cursor_fg: LinearRgba,
                cursor_bg: LinearRgba,
                foreground: LinearRgba,
                cursor_is_default_color: bool,
                white_space: TextureRect,
                filled_box: TextureRect,
                window_is_transparent: bool,
                layers: &'a mut TripleLayerQuadAllocator<'b>,
                error: Option<anyhow::Error>,
            }

            let left_pixel_x = padding_left
                + border.left.get() as f32
                + (pos.left as f32 * self.render_metrics.cell_size.width as f32);

            let mut render = LineRender {
                term_window: self,
                selrange,
                rectangular,
                dims,
                top_pixel_y,
                left_pixel_x,
                pos,
                pane_id,
                cursor: &cursor,
                palette: &palette,
                cursor_border_color,
                selection_fg,
                selection_bg,
                cursor_fg,
                default_bg,
                cursor_bg,
                foreground,
                cursor_is_default_color,
                white_space,
                filled_box,
                window_is_transparent,
                layers,
                error: None,
            };

            impl<'a, 'b> LineRender<'a, 'b> {
                fn render_line(
                    &mut self,
                    stable_top: StableRowIndex,
                    line_idx: usize,
                    line: &&mut Line,
                ) -> anyhow::Result<()> {
                    let stable_row = stable_top + line_idx as StableRowIndex;
                    let selrange = self
                        .selrange
                        .map_or(0..0, |sel| sel.cols_for_row(stable_row, self.rectangular));
                    // Constrain to the pane width!
                    let selrange = selrange.start..selrange.end.min(self.dims.cols);

                    let (cursor, composing, password_input) = if self.cursor.y == stable_row {
                        (
                            Some(CursorProperties {
                                position: StableCursorPosition {
                                    y: 0,
                                    ..*self.cursor
                                },
                                dead_key_or_leader: self.term_window.dead_key_status
                                    != DeadKeyStatus::None
                                    || self.term_window.leader_is_active(),
                                cursor_fg: self.cursor_fg,
                                cursor_bg: self.cursor_bg,
                                cursor_border_color: self.cursor_border_color,
                                cursor_is_default_color: self.cursor_is_default_color,
                            }),
                            match (self.pos.is_active, &self.term_window.dead_key_status) {
                                (true, DeadKeyStatus::Composing(composing)) => {
                                    Some(composing.to_string())
                                }
                                _ => None,
                            },
                            if self.term_window.config.detect_password_input {
                                match self.pos.pane.get_metadata() {
                                    Value::Object(obj) => {
                                        match obj.get(&Value::String("password_input".to_string()))
                                        {
                                            Some(Value::Bool(b)) => *b,
                                            _ => false,
                                        }
                                    }
                                    _ => false,
                                }
                            } else {
                                false
                            },
                        )
                    } else {
                        (None, None, false)
                    };

                    let shape_hash = self.term_window.shape_hash_for_line(line);

                    let quad_key = LineQuadCacheKey {
                        pane_id: self.pane_id,
                        password_input,
                        pane_is_active: self.pos.is_active,
                        config_generation: self.term_window.config.generation(),
                        shape_generation: self.term_window.shape_generation,
                        quad_generation: self.term_window.quad_generation,
                        composing: composing.clone(),
                        selection: selrange.clone(),
                        cursor,
                        shape_hash,
                        top_pixel_y: NotNan::new(self.top_pixel_y).unwrap()
                            + (line_idx + self.pos.top) as f32
                                * self.term_window.render_metrics.cell_size.height as f32,
                        left_pixel_x: NotNan::new(self.left_pixel_x).unwrap(),
                        phys_line_idx: line_idx,
                        reverse_video: self.dims.reverse_video,
                    };

                    if let Some(cached_quad) =
                        self.term_window.line_quad_cache.borrow_mut().get(&quad_key)
                    {
                        let expired = cached_quad
                            .expires
                            .map(|i| Instant::now() >= i)
                            .unwrap_or(false);
                        let hover_changed = if cached_quad.invalidate_on_hover_change {
                            !same_hyperlink(
                                cached_quad.current_highlight.as_ref(),
                                self.term_window.current_highlight.as_ref(),
                            )
                        } else {
                            false
                        };
                        if !expired && !hover_changed {
                            cached_quad
                                .layers
                                .apply_to(self.layers)
                                .context("cached_quad.layers.apply_to")?;
                            self.term_window.update_next_frame_time(cached_quad.expires);
                            return Ok(());
                        }
                    }

                    let mut buf = HeapQuadAllocator::default();
                    let next_due = self.term_window.has_animation.borrow_mut().take();

                    let shape_key = LineToEleShapeCacheKey {
                        shape_hash,
                        shape_generation: quad_key.shape_generation,
                        composing: if self.cursor.y == stable_row && self.pos.is_active {
                            if let DeadKeyStatus::Composing(composing) =
                                &self.term_window.dead_key_status
                            {
                                Some((self.cursor.x, composing.to_string()))
                            } else {
                                None
                            }
                        } else {
                            None
                        },
                    };

                    let render_result = self
                        .term_window
                        .render_screen_line(
                            RenderScreenLineParams {
                                top_pixel_y: *quad_key.top_pixel_y,
                                left_pixel_x: self.left_pixel_x,
                                pixel_width: self.dims.cols as f32
                                    * self.term_window.render_metrics.cell_size.width as f32,
                                stable_line_idx: Some(stable_row),
                                line: &line,
                                selection: selrange.clone(),
                                cursor: &self.cursor,
                                palette: &self.palette,
                                dims: &self.dims,
                                config: &self.term_window.config,
                                cursor_border_color: self.cursor_border_color,
                                foreground: self.foreground,
                                is_active: self.pos.is_active,
                                pane: Some(&self.pos.pane),
                                selection_fg: self.selection_fg,
                                selection_bg: self.selection_bg,
                                cursor_fg: self.cursor_fg,
                                cursor_bg: self.cursor_bg,
                                cursor_is_default_color: self.cursor_is_default_color,
                                white_space: self.white_space,
                                filled_box: self.filled_box,
                                window_is_transparent: self.window_is_transparent,
                                default_bg: self.default_bg,
                                font: None,
                                style: None,
                                use_pixel_positioning: self
                                    .term_window
                                    .config
                                    .experimental_pixel_positioning,
                                render_metrics: self.term_window.render_metrics,
                                shape_key: Some(shape_key),
                                password_input,
                            },
                            &mut TripleLayerQuadAllocator::Heap(&mut buf),
                        )
                        .context("render_screen_line")?;

                    let expires = self.term_window.has_animation.borrow().as_ref().cloned();
                    self.term_window.update_next_frame_time(next_due);

                    buf.apply_to(self.layers)
                        .context("HeapQuadAllocator::apply_to")?;

                    let quad_value = LineQuadCacheValue {
                        layers: buf,
                        expires,
                        invalidate_on_hover_change: render_result.invalidate_on_hover_change,
                        current_highlight: if render_result.invalidate_on_hover_change {
                            self.term_window.current_highlight.clone()
                        } else {
                            None
                        },
                    };

                    self.term_window
                        .line_quad_cache
                        .borrow_mut()
                        .put(quad_key, quad_value);

                    Ok(())
                }
            }

            impl<'a, 'b> WithPaneLines for LineRender<'a, 'b> {
                fn with_lines_mut(&mut self, stable_top: StableRowIndex, lines: &mut [&mut Line]) {
                    for (line_idx, line) in lines.iter().enumerate() {
                        if let Err(err) = self.render_line(stable_top, line_idx, line) {
                            self.error.replace(err);
                            return;
                        }
                    }
                }
            }

            pos.pane.with_lines_mut(stable_range.clone(), &mut render);
            if let Some(error) = render.error.take() {
                return Err(error).context("error while calling with_lines_mut");
            }
        }

        // Dedicated cursor pass: draw the (optionally smeared) cursor outside of
        // the per-line quad cache so it can move continuously between cells.
        // See ADR 0001. `render` (which borrowed self/layers) has been dropped
        // with the block above, so self and layers are free again here.
        if pos.is_active && self.config.cursor_smear_duration_ms != 0 {
            // Pane-origin absolute pixel coordinates. left_pixel_x already folds
            // in the pane's horizontal offset (pos.left); top_pixel_y here is only
            // the window content-area top, so add the pane's vertical offset
            // (pos.top) to match. Both axes now carry the pane offset, so
            // cursor_target_rect only has to add the in-pane cell offset.
            let left_pixel_x = padding_left
                + border.left.get() as f32
                + (pos.left as f32 * cell_width);
            let pane_top_pixel_y = top_pixel_y + (pos.top as f32 * cell_height);
            let stable_range = match current_viewport {
                Some(top) => top..top + dims.viewport_rows as StableRowIndex,
                None => dims.physical_top..dims.physical_top + dims.viewport_rows as StableRowIndex,
            };

            // Effective shape: config default resolved against the cursor's own
            // shape. This narrows the trail rect into a bar/underline.
            let shape = config.default_cursor_style.effective_shape(cursor.shape);

            // The per-line pass owns a special cursor here, so the smear stands
            // down: an active IME pre-edit (composition block) or password input
            // (lock glyph). Mirrors the same checks in render_line / screen_line.
            let composing = matches!(self.dead_key_status, DeadKeyStatus::Composing(_))
                || self.leader_is_active();
            let password_input = self.config.detect_password_input
                && match pos.pane.get_metadata() {
                    Value::Object(obj) => matches!(
                        obj.get(&Value::String("password_input".to_string())),
                        Some(Value::Bool(true))
                    ),
                    _ => false,
                };
            let defer_to_per_line = composing || password_input;

            // Resolve the cursor colour for the target cell, matching the plain
            // cursor branch of compute_cell_fg_bg (incl. reverse-video). Read the
            // target cell's fg/bg for the contrast test the reverse path needs.
            let (cell_fg, cell_bg) = {
                let row = cursor.y;
                let (_first, lines) = pos.pane.get_lines(row..row + 1);
                let attrs = lines
                    .first()
                    .and_then(|line| line.get_cell(cursor.x).map(|c| c.attrs().clone()));
                match attrs {
                    Some(attrs) => (
                        palette.resolve_fg(attrs.foreground()).to_linear(),
                        palette.resolve_bg(attrs.background()).to_linear(),
                    ),
                    None => (foreground, default_bg),
                }
            };
            let cursor_color = self.resolve_cursor_smear_color(
                cursor_is_default_color,
                cursor_bg,
                cell_fg,
                cell_bg,
            );

            self.paint_cursor_smear(
                CursorSmearParams {
                    cursor: &cursor,
                    viewport_top: stable_range.start,
                    left_pixel_x,
                    top_pixel_y: pane_top_pixel_y,
                    cell_width,
                    cell_height,
                    shape,
                    defer_to_per_line,
                    cursor_color,
                },
                layers,
            )
            .context("paint_cursor_smear")?;
        }

        /*
        if let Some(zone) = zone {
            // TODO: render a thingy to jump to prior prompt
        }
        */
        metrics::histogram!("paint_pane.lines").record(start.elapsed());
        log::trace!("lines elapsed {:?}", start.elapsed());

        Ok(())
    }

    /// Resolve the colour the smear should paint the cursor in, mirroring the
    /// plain-cursor branch of `compute_cell_fg_bg` (no IME / selection / bell,
    /// which the smear never owns). For every shape the cursor colour reduces to
    /// the same choice: reverse-video uses the target cell's foreground, and
    /// otherwise the palette `cursor_bg`. `cell_fg`/`cell_bg` are the target
    /// cell's resolved colours, needed for the force_reverse_video_cursor
    /// contrast test. See ADR 0001.
    fn resolve_cursor_smear_color(
        &self,
        cursor_is_default_color: bool,
        cursor_bg: LinearRgba,
        cell_fg: LinearRgba,
        cell_bg: LinearRgba,
    ) -> LinearRgba {
        let reverse = self.config.force_reverse_video_cursor
            && cursor_is_default_color
            && cell_fg.contrast_ratio(&cell_bg)
                >= self.config.reverse_video_cursor_min_contrast;
        if reverse {
            cell_fg
        } else {
            cursor_bg
        }
    }

    /// Draw the cursor as a dedicated, non-cached quad so it can move
    /// continuously between cells (the Neovide-style "trail" animation). When
    /// the smear is enabled this pass owns the *entire* cursor: it draws the
    /// shape (block/bar/underline narrowed by `cursor_target_rect`) in the
    /// resolved cursor colour, both moving and at rest, so the trail is the
    /// cursor stretched rather than a detached block. The four corners are
    /// animated by independent critically damped springs and drawn as a filled
    /// quadrilateral. See ADR 0001.
    fn paint_cursor_smear(
        &mut self,
        params: CursorSmearParams,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        use termwiz::surface::CursorVisibility;

        let target = self.cursor_target_rect(&params);

        // While the per-line pass owns a special cursor (IME composition block or
        // password lock glyph), stand down so we don't paint a second cursor over
        // it, and snap the trail to the current spot so that interlude isn't
        // mistaken for movement once we resume.
        let hidden = params.cursor.visibility != CursorVisibility::Visible;
        if hidden || params.defer_to_per_line {
            self.cursor_smear_pos
                .borrow_mut()
                .should_snap(params.viewport_top);
            self.cursor_trail.borrow_mut().snap_to(target);
            *self.cursor_trail_last_frame.borrow_mut() = None;
            return Ok(());
        }

        // On the first frame or after a scroll, snap the trail rather than
        // animating, so scrolling is not mistaken for cursor movement.
        if self
            .cursor_smear_pos
            .borrow_mut()
            .should_snap(params.viewport_top)
        {
            self.cursor_trail.borrow_mut().snap_to(target);
        }

        let dt = self.cursor_trail_dt();
        let base_len = self.config.cursor_smear_duration_ms as f32 / 1000.0;
        let trail_size = self.config.cursor_trail_size;

        let animating = self
            .cursor_trail
            .borrow_mut()
            .update(target, dt, base_len, trail_size);

        if animating {
            // Re-render next frame at the configured animation rate.
            let fps = self.config.animation_fps.max(1) as u64;
            let next = Instant::now() + Duration::from_millis(1000 / fps);
            self.update_next_frame_time(Some(next));
        } else {
            *self.cursor_trail_last_frame.borrow_mut() = None;
        }

        // Always draw: this pass owns the cursor, so even at rest the (settled)
        // quad is the cursor's resting shape. No hand-off to the per-line pass,
        // hence no block->bar popping.
        let corners = self.cursor_trail.borrow().corner_points();
        self.draw_cursor_quad(corners, params.cursor_color, layers)
    }

    /// Seconds elapsed since the previous trail frame, clamped to avoid large
    /// jumps after the animation was idle. Updates the stored timestamp.
    fn cursor_trail_dt(&self) -> f32 {
        let now = Instant::now();
        let mut last = self.cursor_trail_last_frame.borrow_mut();
        let dt = match *last {
            Some(prev) => (now - prev).as_secs_f32().min(0.1),
            None => 0.0,
        };
        *last = Some(now);
        dt
    }

    /// Draw the four animated cursor corners as a filled quadrilateral. Corners
    /// are in window pixels, perimeter order (TL, TR, BR, BL).
    fn draw_cursor_quad(
        &self,
        corners: [(f32, f32); 4],
        color: LinearRgba,
        layers: &mut TripleLayerQuadAllocator,
    ) -> anyhow::Result<()> {
        let left_offset = self.dimensions.pixel_width as f32 / 2.;
        let top_offset = self.dimensions.pixel_height as f32 / 2.;
        let gl = |c: (f32, f32)| (c.0 - left_offset, c.1 - top_offset);

        let gl_state = self.render_state.as_ref().unwrap();
        let filled_box = gl_state.util_sprites.filled_box.texture_coords();

        let mut quad = layers.allocate(0).context("layers.allocate for cursor")?;
        quad.set_position_quad(gl(corners[0]), gl(corners[1]), gl(corners[2]), gl(corners[3]));
        quad.set_texture(filled_box);
        quad.set_is_background();
        quad.set_fg_color(color);
        quad.set_hsv(None);
        Ok(())
    }

    /// The cursor's current target rectangle in window-relative screen pixels,
    /// narrowed to the cursor shape. Block fills the cell; Bar is a thin column
    /// on the left edge; Underline is a thin row on the bottom edge. The trail
    /// springs act on this rect, so the trail is the shape stretched (a bar
    /// trails as a bar, not as a block). See ADR 0001.
    fn cursor_target_rect(&self, params: &CursorSmearParams) -> CursorPixelRect {
        let row_offset = params.cursor.y - params.viewport_top;
        let cell_x = params.left_pixel_x + params.cursor.x as f32 * params.cell_width;
        let cell_y = params.top_pixel_y + row_offset as f32 * params.cell_height;

        // Thickness of bar/underline, scaled to the configured cursor sprite
        // weight so the resting shape roughly matches the per-line cursor. At
        // least one pixel so it never vanishes.
        let thickness = (self.render_metrics.underline_height as f32).max(1.0);

        match params.shape {
            CursorShape::BlinkingBar | CursorShape::SteadyBar => CursorPixelRect {
                x: cell_x,
                y: cell_y,
                width: thickness,
                height: params.cell_height,
            },
            CursorShape::BlinkingUnderline | CursorShape::SteadyUnderline => CursorPixelRect {
                x: cell_x,
                y: cell_y + params.cell_height - thickness,
                width: params.cell_width,
                height: thickness,
            },
            // Block (and Default) fill the whole cell.
            _ => CursorPixelRect {
                x: cell_x,
                y: cell_y,
                width: params.cell_width,
                height: params.cell_height,
            },
        }
    }

    pub fn build_pane(&mut self, pos: &PositionedPane) -> anyhow::Result<ComputedElement> {
        // First compute the bounds for the pane background

        let cell_width = self.render_metrics.cell_size.width as f32;
        let cell_height = self.render_metrics.cell_size.height as f32;
        let (padding_left, padding_top) = self.padding_left_top();
        let tab_bar_height = if self.show_tab_bar {
            self.tab_bar_pixel_height()?
        } else {
            0.
        };
        let (top_bar_height, _bottom_bar_height) = if self.config.tab_bar_at_bottom {
            (0.0, tab_bar_height)
        } else {
            (tab_bar_height, 0.0)
        };

        let border = self.get_os_border();
        let top_pixel_y = top_bar_height + padding_top + border.top.get() as f32;

        // We want to fill out to the edges of the splits
        let (x, width_delta) = if pos.left == 0 {
            (
                0.,
                padding_left + border.left.get() as f32 + (cell_width / 2.0),
            )
        } else {
            (
                padding_left + border.left.get() as f32 - (cell_width / 2.0)
                    + (pos.left as f32 * cell_width),
                cell_width,
            )
        };

        let (y, height_delta) = if pos.top == 0 {
            (
                (top_pixel_y - padding_top),
                padding_top + (cell_height / 2.0),
            )
        } else {
            (
                top_pixel_y + (pos.top as f32 * cell_height) - (cell_height / 2.0),
                cell_height,
            )
        };

        let background_rect = euclid::rect(
            x,
            y,
            // Go all the way to the right edge if we're right-most
            if pos.left + pos.width >= self.terminal_size.cols as usize {
                self.dimensions.pixel_width as f32 - x
            } else {
                (pos.width as f32 * cell_width) + width_delta
            },
            // Go all the way to the bottom if we're bottom-most
            if pos.top + pos.height >= self.terminal_size.rows as usize {
                self.dimensions.pixel_height as f32 - y
            } else {
                (pos.height as f32 * cell_height) + height_delta as f32
            },
        );

        // Bounds for the terminal cells
        let content_rect = euclid::rect(
            padding_left + border.left.get() as f32 - (cell_width / 2.0)
                + (pos.left as f32 * cell_width),
            top_pixel_y + (pos.top as f32 * cell_height) - (cell_height / 2.0),
            pos.width as f32 * cell_width,
            pos.height as f32 * cell_height,
        );

        let palette = pos.pane.palette();

        // TODO: visual bell background layer
        // TODO: scrollbar

        Ok(ComputedElement {
            item_type: None,
            zindex: 0,
            bounds: background_rect,
            border: PixelDimension::default(),
            border_rect: background_rect,
            border_corners: None,
            colors: ElementColors {
                border: BorderColor::default(),
                bg: if self.window_background.is_empty() {
                    palette
                        .background
                        .to_linear()
                        .mul_alpha(self.config.window_background_opacity)
                        .into()
                } else {
                    InheritableColor::Inherited
                },
                text: InheritableColor::Inherited,
            },
            hover_colors: None,
            padding: background_rect,
            content_rect,
            baseline: 1.0,
            content: ComputedElementContent::Children(vec![]),
        })
    }
}
