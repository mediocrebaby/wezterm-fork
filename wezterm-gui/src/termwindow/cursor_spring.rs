//! Neovide-style cursor trail animation.
//!
//! The cursor is modelled as four corners, each animating independently toward
//! its destination with a critically damped spring. Corners aligned against the
//! direction of travel are given a longer animation so they lag behind, which
//! stretches the cursor quad into a trail. The animated corners are then drawn
//! as a plain filled quadrilateral (see ADR 0001).
//!
//! Ported from neovide's `cursor_renderer`; the spring integration matches
//! neovide's `CriticallyDampedSpringAnimation`.

use crate::termwindow::prevcursor::CursorPixelRect;

/// A single axis of critically damped spring motion. `position` is the residual
/// distance still to travel toward the target (target is implicitly 0), so the
/// rendered coordinate is `target + position`.
#[derive(Clone, Copy, Default)]
struct Spring {
    position: f32,
    velocity: f32,
}

impl Spring {
    /// Advance the spring by `dt` seconds. `animation_length` is the nominal
    /// time to settle. Returns true while still animating. Mirrors neovide's
    /// analytic critically damped update.
    fn update(&mut self, dt: f32, animation_length: f32) -> bool {
        // Critically damped: zeta = 1. omega controls how quickly we settle.
        let omega = 4.0 / animation_length.max(1e-4);

        let a = self.position;
        let b = self.position * omega + self.velocity;
        let c = (-omega * dt).exp();

        self.position = (a + b * dt) * c;
        self.velocity = c * (-a * omega - b * dt * omega + b);

        // Consider the spring settled once the residual is sub-pixel.
        self.position.abs() > 0.1 || self.velocity.abs() > 0.1
    }

    /// Re-target the spring so that it now needs to travel `residual` more.
    fn retarget(&mut self, residual: f32) {
        self.position = residual;
    }
}

/// One animated cursor corner.
#[derive(Clone, Copy)]
struct Corner {
    /// Offset of this corner from the cursor rect center, as a fraction of the
    /// rect size: each component is -0.5 or +0.5.
    rel: (f32, f32),
    spring_x: Spring,
    spring_y: Spring,
    animation_length: f32,
}

impl Corner {
    fn new(rel: (f32, f32)) -> Self {
        Self {
            rel,
            spring_x: Spring::default(),
            spring_y: Spring::default(),
            animation_length: 0.0,
        }
    }

    /// Target pixel position of this corner for the given cursor rect.
    fn target(&self, rect: &CursorPixelRect) -> (f32, f32) {
        (
            rect.x + rect.width * (0.5 + self.rel.0),
            rect.y + rect.height * (0.5 + self.rel.1),
        )
    }

    /// Current animated pixel position = target + residual.
    fn current(&self, rect: &CursorPixelRect) -> (f32, f32) {
        let (tx, ty) = self.target(rect);
        (tx + self.spring_x.position, ty + self.spring_y.position)
    }
}

/// The four corners of the cursor trail. Order: top-left, top-right,
/// bottom-right, bottom-left (perimeter order for the fill quad).
pub struct CursorTrail {
    corners: [Corner; 4],
    target: Option<CursorPixelRect>,
}

impl CursorTrail {
    pub fn new() -> Self {
        Self {
            corners: [
                Corner::new((-0.5, -0.5)),
                Corner::new((0.5, -0.5)),
                Corner::new((0.5, 0.5)),
                Corner::new((-0.5, 0.5)),
            ],
            target: None,
        }
    }

    /// Re-aim the springs at a new cursor rect, ranking corners by how aligned
    /// they are with the direction of travel so trailing corners lag. Called
    /// when the cursor moves to a new cell. `base_len`/`fast_len` are the
    /// trailing/leading animation lengths in seconds; `trail_size` scales how
    /// much the trailing corners lag behind the leading ones.
    fn retarget(&mut self, new_rect: CursorPixelRect, base_len: f32, trail_size: f32) {
        let old = match self.target {
            Some(rect) => rect,
            None => new_rect,
        };
        let old_center = (old.x + old.width / 2.0, old.y + old.height / 2.0);
        let new_center = (
            new_rect.x + new_rect.width / 2.0,
            new_rect.y + new_rect.height / 2.0,
        );
        let travel = (new_center.0 - old_center.0, new_center.1 - old_center.1);
        let travel_len = (travel.0 * travel.0 + travel.1 * travel.1).sqrt();

        for corner in &mut self.corners {
            // Preserve the corner's current animated position as the new
            // residual relative to the new target, so motion is continuous.
            let (cx, cy) = corner.current(&old);
            let (tx, ty) = corner.target(&new_rect);
            corner.spring_x.retarget(cx - tx);
            corner.spring_y.retarget(cy - ty);

            // Rank corners by alignment with the travel direction: a corner on
            // the leading edge (its outward direction agrees with travel) moves
            // fast; a trailing corner moves slow, producing the stretch.
            corner.animation_length = if travel_len < 1e-3 {
                base_len
            } else {
                let dir = (corner.rel.0, corner.rel.1);
                let dot = (dir.0 * travel.0 + dir.1 * travel.1) / travel_len;
                // dot in [-0.707, 0.707] for unit-ish corners; map so leading
                // corners (dot > 0) settle in `base_len * (1 - trail_size)` and
                // trailing corners (dot < 0) take the full `base_len`.
                let lead = (dot + 0.5).clamp(0.0, 1.0); // 1 = leading, 0 = trailing
                base_len * (1.0 - trail_size * lead)
            };
        }
        self.target = Some(new_rect);
    }

    /// Advance all corner springs by `dt` seconds toward `rect`. If `rect`
    /// differs from the current target the springs are re-aimed first.
    /// Returns true while any corner is still animating.
    pub fn update(
        &mut self,
        rect: CursorPixelRect,
        dt: f32,
        base_len: f32,
        trail_size: f32,
    ) -> bool {
        let moved = self.target.map_or(true, |t| t != rect);
        if moved {
            self.retarget(rect, base_len, trail_size);
        }

        let mut animating = false;
        for corner in &mut self.corners {
            let len = corner.animation_length.max(1e-4);
            animating |= corner.spring_x.update(dt, len);
            animating |= corner.spring_y.update(dt, len);
        }
        animating
    }

    /// The four animated corner positions (perimeter order) for the fill quad.
    pub fn corner_points(&self) -> [(f32, f32); 4] {
        let rect = self.target.unwrap_or(CursorPixelRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        });
        [
            self.corners[0].current(&rect),
            self.corners[1].current(&rect),
            self.corners[2].current(&rect),
            self.corners[3].current(&rect),
        ]
    }

    /// Reset to a settled state at `rect` with no trail (e.g. on scroll).
    pub fn snap_to(&mut self, rect: CursorPixelRect) {
        for corner in &mut self.corners {
            corner.spring_x = Spring::default();
            corner.spring_y = Spring::default();
            corner.animation_length = 0.0;
        }
        self.target = Some(rect);
    }

    /// Drop any remembered target and in-flight spring state. The next use will
    /// start from a clean slate rather than resuming an old trail.
    pub fn reset(&mut self) {
        for corner in &mut self.corners {
            corner.spring_x = Spring::default();
            corner.spring_y = Spring::default();
            corner.animation_length = 0.0;
        }
        self.target = None;
    }
}
