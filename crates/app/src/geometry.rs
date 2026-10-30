//! Shape sizes per state and their window position on screen.
//!
//! All shapes sit at the top center of a "canvas" the size of the expanded panel (plus shoulders).
//! The OS window is only as big as the visible shape, so the area outside it stays clickable.

use crate::state::Visual;

/// Shape size in logical pixels (96 DPI).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub w: f32,
    pub h: f32,
    pub r: f32,
}

impl Shape {
    pub fn area(&self) -> f32 {
        self.w * self.h
    }

    /// A shape that is too short gets no shoulders.
    pub fn is_solid(&self) -> bool {
        self.h >= 2.0 * SHOULDER
    }
}

/// Rectangle in physical screen pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }

    pub fn covers(&self, o: &Rect) -> bool {
        self.x <= o.x && self.y <= o.y && self.right() >= o.right() && self.bottom() >= o.bottom()
    }

    /// Smallest rectangle containing both.
    pub fn union(&self, o: &Rect) -> Rect {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect { x, y, w: self.right().max(o.right()) - x, h: self.bottom().max(o.bottom()) - y }
    }
}

pub const CANVAS_W: f32 = 440.0;
/// Tallest panel height; longer content scrolls. See `paint::layout`.
pub const CANVAS_H: f32 = 300.0;
pub const PANEL_MIN_H: f32 = 150.0;
/// Width of the permission, question and plan cards. Height follows the content up to `CANVAS_H`; the rest scrolls.
pub const CARD_W: f32 = 380.0;
pub const GLANCE_MIN_W: f32 = 160.0;
pub const GLANCE_MAX_W: f32 = 400.0;
/// Radius of the concave curve at the top left and right, where the notch blends into the screen edge.
pub const SHOULDER: f32 = 8.0;
/// Extra room in the canvas for spring animation overshoot.
pub const OVERSHOOT: f32 = 12.0;

pub fn shape_for(v: &Visual, glance_w: f32, hovering: bool, panel_h: f32) -> Shape {
    match v {
        // Only Nochi's eyes peek out above the window occupying the notch area.
        Visual::Hidden | Visual::Peek => Shape { w: 60.0, h: 20.0, r: 10.0 },
        // A 28 px height fits in the Windows title bar. When the cursor is over it, the notch grows and
        // shows details (next task, agent activity).
        Visual::Pill if hovering => Shape { w: 300.0, h: 52.0, r: 22.0 },
        Visual::Pill => Shape { w: 120.0, h: 28.0, r: 14.0 },
        Visual::Live if hovering => Shape { w: 340.0, h: 56.0, r: 24.0 },
        Visual::Live => Shape { w: 210.0, h: 28.0, r: 14.0 },
        Visual::Focus if hovering => Shape { w: 300.0, h: 52.0, r: 22.0 },
        Visual::Focus => Shape { w: 120.0, h: 28.0, r: 14.0 },
        // Permission card is at least 104 px; its height follows the content (`panel_h`), long text scrolls.
        Visual::Permission => Shape { w: CARD_W, h: panel_h.clamp(104.0, CANVAS_H), r: 26.0 },
        Visual::Glance(_) => Shape { w: glance_w.clamp(GLANCE_MIN_W, GLANCE_MAX_W), h: 32.0, r: 16.0 },
        Visual::Chip => Shape { w: 390.0, h: 56.0, r: 24.0 },
        Visual::Expanded => Shape { w: CANVAS_W, h: panel_h.clamp(PANEL_MIN_H, CANVAS_H), r: 22.0 },
    }
}

/// Monitor the notch appears on, with its DPI scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub monitor: Rect,
    pub scale: f32,
}

impl Placement {
    pub fn canvas(&self) -> Rect {
        let w = ((CANVAS_W + 2.0 * (SHOULDER + OVERSHOOT)) * self.scale).ceil() as i32;
        let h = ((CANVAS_H + OVERSHOOT) * self.scale).ceil() as i32;
        Rect { x: self.monitor.x + (self.monitor.w - w) / 2, y: self.monitor.y, w, h }
    }

    /// Window during animation: widened to the sides and down so spring overshoot is not clipped.
    pub fn while_animating(&self, r: &Rect) -> Rect {
        let c = self.canvas();
        let pad = (OVERSHOOT * self.scale).ceil() as i32;
        let x = (r.x - pad).max(c.x);
        let right = (r.right() + pad).min(c.right());
        Rect { x, y: r.y, w: right - x, h: (r.h + pad).min(c.h) }
    }

    /// Window during animation with a fixed width (e.g. switching panel tabs): only made taller
    /// downward. Its left side does not shift, so when the animation ends the content does not jump
    /// sideways (window shifts and Composition content do not always appear in the same frame).
    pub fn while_resizing(&self, current: &Rect, target: &Rect) -> Rect {
        let c = self.canvas();
        let pad = (OVERSHOOT * self.scale).ceil() as i32;
        Rect { h: (current.h.max(target.h) + pad).min(c.h), ..*target }
    }

    /// Window exactly the size of the shape plus its shoulders, at the top center of the monitor, so the surrounding area
    /// stays clickable.
    pub fn window_for(&self, s: &Shape) -> Rect {
        let c = self.canvas();
        let w = (((s.w + 2.0 * SHOULDER) * self.scale).ceil() as i32).min(c.w);
        let h = ((s.h * self.scale).ceil() as i32).min(c.h);
        Rect { x: c.x + (c.w - w) / 2, y: c.y, w, h }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FHD: Rect = Rect { x: 0, y: 0, w: 1920, h: 1080 };

    #[test]
    fn pill_is_centered_at_top() {
        let p = Placement { monitor: FHD, scale: 1.0 };
        assert_eq!(p.canvas(), Rect { x: 720, y: 0, w: 480, h: 312 });
        assert_eq!(p.window_for(&shape_for(&Visual::Pill, 0.0, false, 0.0)), Rect { x: 892, y: 0, w: 136, h: 28 });
        assert_eq!(p.window_for(&shape_for(&Visual::Peek, 0.0, false, 0.0)), Rect { x: 922, y: 0, w: 76, h: 20 });
    }

    #[test]
    fn animation_window_stays_inside_canvas() {
        let p = Placement { monitor: FHD, scale: 1.0 };
        let pill = p.window_for(&shape_for(&Visual::Pill, 0.0, false, 0.0));
        assert_eq!(p.while_animating(&pill), Rect { x: 880, y: 0, w: 160, h: 40 });
        let panel = p.window_for(&shape_for(&Visual::Expanded, 0.0, false, 250.0));
        assert_eq!(p.while_animating(&panel), Rect { x: 720, y: 0, w: 480, h: 262 });
        // Same width: only made taller, left side stays.
        let short = p.window_for(&shape_for(&Visual::Expanded, 0.0, false, 150.0));
        assert_eq!(p.while_resizing(&panel, &short), Rect { x: panel.x, y: 0, w: panel.w, h: 262 });
        let tallest = p.window_for(&shape_for(&Visual::Expanded, 0.0, false, 9999.0));
        assert_eq!(p.while_animating(&tallest), p.canvas());
    }

    #[test]
    fn scales_with_dpi_on_secondary_monitor() {
        let p = Placement { monitor: Rect { x: 1920, y: -200, w: 2560, h: 1440 }, scale: 1.5 };
        let r = p.window_for(&shape_for(&Visual::Pill, 0.0, false, 0.0));
        assert_eq!((r.w, r.h, r.y), (204, 42, -200));
        assert_eq!(r.x - p.canvas().x, (p.canvas().w - 204) / 2);
    }

    #[test]
    fn hover_grows_pill_and_live_only() {
        let pill = shape_for(&Visual::Pill, 0.0, false, 0.0);
        assert!(shape_for(&Visual::Pill, 0.0, true, 0.0).area() > pill.area());
        assert!(shape_for(&Visual::Live, 0.0, true, 0.0).area() > shape_for(&Visual::Live, 0.0, false, 0.0).area());
        assert!(shape_for(&Visual::Focus, 0.0, true, 0.0).area() > shape_for(&Visual::Focus, 0.0, false, 0.0).area());
        assert_eq!(shape_for(&Visual::Peek, 0.0, true, 0.0), shape_for(&Visual::Peek, 0.0, false, 0.0));
        assert_eq!(shape_for(&Visual::Permission, 0.0, true, 0.0), shape_for(&Visual::Permission, 0.0, false, 0.0));
    }

    #[test]
    fn glance_width_is_clamped() {
        assert_eq!(shape_for(&Visual::Glance("x".into()), 10.0, false, 0.0).w, GLANCE_MIN_W);
        assert_eq!(shape_for(&Visual::Glance("x".into()), 9999.0, false, 0.0).w, GLANCE_MAX_W);
    }

    #[test]
    fn rect_relations() {
        let zone = Rect { x: 857, y: 0, w: 206, h: 34 };
        let maximized = Rect { x: 0, y: 0, w: 1920, h: 1032 };
        let left_half = Rect { x: 0, y: 0, w: 960, h: 1032 };
        let below = Rect { x: 0, y: 40, w: 1920, h: 900 };
        assert!(maximized.intersects(&zone) && left_half.intersects(&zone));
        assert!(!below.intersects(&zone));
        assert!(!maximized.covers(&FHD) && Rect { x: 0, y: 0, w: 1920, h: 1080 }.covers(&FHD));
        assert_eq!(zone.union(&Rect { x: 600, y: 0, w: 720, h: 428 }), Rect { x: 600, y: 0, w: 720, h: 428 });
    }
}
