use mux::renderable::StableCursorPosition;
use std::time::Instant;
use wezterm_term::StableRowIndex;

#[derive(Clone)]
pub struct PrevCursorPos {
    pos: StableCursorPosition,
    when: Instant,
}

impl PrevCursorPos {
    pub fn new() -> Self {
        PrevCursorPos {
            pos: StableCursorPosition::default(),
            when: Instant::now(),
        }
    }

    /// Make the cursor look like it moved
    pub fn bump(&mut self) {
        self.when = Instant::now();
    }

    /// Update the cursor position if its different
    pub fn update(&mut self, newpos: &StableCursorPosition) {
        if &self.pos != newpos {
            self.pos = *newpos;
            self.when = Instant::now();
        }
    }

    /// When did the cursor last move?
    pub fn last_cursor_movement(&self) -> Instant {
        self.when
    }
}

/// A cursor rectangle expressed in window-relative screen pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorPixelRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Detects viewport scrolls so the cursor trail can tell genuine cursor
/// movement apart from scrolling.
///
/// The cursor's stable row index does not change when the viewport scrolls, but
/// its on-screen pixel position does. If we let the spring animate across that
/// pixel jump it would smear on every scroll. We therefore watch the viewport
/// top and report a scroll whenever it changes so the caller can snap the trail
/// instead of animating it (see ADR 0001).
#[derive(Clone)]
pub struct CursorSmearState {
    prev_viewport_top: Option<StableRowIndex>,
}

impl CursorSmearState {
    pub fn new() -> Self {
        Self {
            prev_viewport_top: None,
        }
    }

    /// Record the viewport top for this frame and return true if the trail
    /// should snap (the first frame, or the viewport scrolled) rather than
    /// animate. Must be called once per painted frame for the active pane.
    pub fn should_snap(&mut self, viewport_top: StableRowIndex) -> bool {
        let snap = self.prev_viewport_top != Some(viewport_top);
        self.prev_viewport_top = Some(viewport_top);
        snap
    }
}
