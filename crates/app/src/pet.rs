//! Nochi, the pet that lives in the notch: a pair of white eyes with a soft glow.
//!
//! The eyes are Composition vector shapes, so all their motion (blink, glance, reading, hop)
//! runs in DWM. Continuously repeating animations are only used in transient states (agent working,
//! glance, permission). When idle, blinks are triggered by an app timer every few seconds as one-shot
//! animations, so DWM doesn't redraw every frame all day.
//!
//! When the agent works while music plays, Nochi nods gently to the beat instead of reading.

use windows::{core::*, Foundation::TimeSpan, UI::Color, UI::Composition::*};
use windows_numerics::{Vector2, Vector3};

/// Eye drawing box (logical). The face center is at its middle.
const BOX_W: f32 = 48.0;
const BOX_H: f32 = 28.0;
const EYE: Color = Color { A: 255, R: 245, G: 245, B: 247 };
/// Duration of the eye shape change between moods.
const MORPH_MS: u32 = 180;
/// Length of one blink. The eyes are fully closed for about 100 ms in the middle, so it stays visible even if
/// DWM skips a few frames when busy (e.g. while a video plays).
const BLINK_MS: u32 = 380;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mood {
    /// Blinks and occasionally glances.
    Idle,
    /// Agent is running a tool: small eyes move left and right as if reading.
    Focused,
    /// Between tools: glances to the upper right.
    Thinking,
    /// Cursor approaching: turns to look down, toward the cursor.
    Curious,
    /// "^ ^" eyes; hops if `hop`.
    Happy { hop: bool },
    /// Needs you: round eyes widen and pulse.
    Alert,
    /// Late at night: eyes closed.
    Sleepy,
    /// Focus pomodoro: eyes narrow calmly, looking down at the work. No repeating motion
    /// (a 25-minute session must not make DWM draw every frame); only blinks.
    Zen,
    /// Dragged but not willing to move yet: eyes narrow in annoyance.
    Reluctant,
    /// Has given up and is being carried: wide round eyes glancing toward the direction of motion.
    Carried,
}

/// Face size to fit its container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// Peeking above the window (20 px notch).
    Peek,
    /// 28 px pill.
    Pill,
    /// Hovered notch.
    Large,
    /// Inside the 22 px chrome ring.
    Tiny,
    /// Inside a 28-36 px badge or tile.
    Badge,
}

impl Size {
    fn scale(self) -> f32 {
        match self {
            Size::Peek => 0.78,
            Size::Pill => 1.0,
            Size::Large => 1.2,
            Size::Tiny => 0.62,
            Size::Badge => 0.85,
        }
    }
}

enum Eyes {
    /// Width, height.
    Rect(f32, f32),
    /// Radius of the "^" arc.
    Arc(f32),
}

/// Eye shape and half the distance between their centers, before scaling by `Size`.
fn face(mood: Mood) -> (Eyes, f32) {
    match mood {
        Mood::Idle | Mood::Thinking | Mood::Curious => (Eyes::Rect(5.0, 9.0), 5.5),
        Mood::Focused => (Eyes::Rect(5.0, 5.0), 5.5),
        Mood::Happy { .. } => (Eyes::Arc(3.6), 6.0),
        Mood::Alert => (Eyes::Rect(8.0, 8.0), 6.5),
        Mood::Sleepy => (Eyes::Rect(9.0, 2.2), 6.5),
        Mood::Zen => (Eyes::Rect(6.0, 3.6), 5.5),
        Mood::Reluctant => (Eyes::Rect(8.0, 2.6), 6.0),
        Mood::Carried => (Eyes::Rect(7.0, 7.0), 6.0),
    }
}

fn ticks(ms: u32) -> TimeSpan {
    TimeSpan { Duration: ms as i64 * 10_000 }
}

fn v3(x: f32, y: f32) -> Vector3 {
    Vector3 { X: x, Y: y, Z: 0.0 }
}

pub struct Pet {
    compositor: Compositor,
    /// Scaled to DPI, so all its children work in logical pixels.
    pub root: ContainerVisual,
    /// Smoothly returns the eyes to center when the motion changes.
    motion: ContainerVisual,
    /// Offset animations (glance, reading, hop, nod). Separate from `motion` so that a
    /// new motion can start from zero right away while the remains of the old one are damped in `motion`.
    wobble: ContainerVisual,
    /// Scale animations (blink, pulse).
    blinker: ContainerVisual,
    eyes: ShapeVisual,
    rects: [CompositionRoundedRectangleGeometry; 2],
    arcs: [CompositionEllipseGeometry; 2],
    rect_shapes: [CompositionSpriteShape; 2],
    arc_shapes: [CompositionSpriteShape; 2],
    /// Glow: two outlines per eye (outer, inner) on the eye geometry, eye 0 then 1.
    rect_halos: Vec<CompositionSpriteShape>,
    arc_halos: Vec<CompositionSpriteShape>,
    brush: CompositionColorBrush,
    clip: InsetClip,
    ease: CubicBezierEasingFunction,
    /// Size, mood, and whether music is playing.
    state: Option<(Size, Mood, bool)>,
    ticks: u32,
    /// Square eyes (not the arc) are currently installed; only this shape can be morphed.
    rects_shown: bool,
    /// Nochi's position: `r` = distance from the notch shape's left edge, `y` = distance from the top (canvas pixels).
    /// The `root` offset follows the shape's left edge via ExpressionAnimation, so while the shape
    /// animates Nochi stays inside it.
    place: CompositionPropertySet,
    /// Last `place` value; `None` when hidden.
    placed: Option<(f32, f32)>,
    /// When the last blink and face change started. A blink doesn't start in the middle of a face change
    /// or another blink; both animate the same eye shape, so the later one would
    /// cut off the first and the blink would look only half done.
    blink_at: std::cell::Cell<Option<std::time::Instant>>,
    morph_at: std::cell::Cell<Option<std::time::Instant>>,
}

impl Pet {
    /// `shape`: the notch shape's geometry, whose left edge Nochi follows.
    pub fn create(compositor: &Compositor, shape: &CompositionRoundedRectangleGeometry) -> Result<Self> {
        let root = compositor.CreateContainerVisual()?;
        root.SetOpacity(0.0)?;
        let place = compositor.CreatePropertySet()?;
        place.InsertScalar(h!("r"), 0.0)?;
        place.InsertScalar(h!("y"), 0.0)?;
        // Rounded to whole pixels: the glow layer beneath is rendered to a texture that is always aligned
        // with pixels, so fractional positions make the eyes jitter while moving.
        let follow = compositor.CreateExpressionAnimationWithExpression(h!("Vector3(Round(g.Offset.X + p.r), Round(p.y), 0)"))?;
        follow.SetReferenceParameter(h!("g"), shape)?;
        follow.SetReferenceParameter(h!("p"), &place)?;
        root.StartAnimation(h!("Offset"), &follow)?;

        // The glow around the eyes is made from transparent outlines on the same eye shape (see
        // `halos`), not LayerVisual + DropShadow: that layer is re-rendered to a texture and blurred
        // every frame, and when DWM is busy (scroll, screen recording, video, fullscreen) the eyes jitter.
        let motion = compositor.CreateContainerVisual()?;
        motion.SetSize(Vector2 { X: BOX_W, Y: BOX_H })?;
        root.Children()?.InsertAtTop(&motion)?;
        let wobble = compositor.CreateContainerVisual()?;
        wobble.SetSize(Vector2 { X: BOX_W, Y: BOX_H })?;
        motion.Children()?.InsertAtTop(&wobble)?;
        let blinker = compositor.CreateContainerVisual()?;
        blinker.SetSize(Vector2 { X: BOX_W, Y: BOX_H })?;
        wobble.Children()?.InsertAtTop(&blinker)?;
        let eyes = compositor.CreateShapeVisual()?;
        eyes.SetSize(Vector2 { X: BOX_W, Y: BOX_H })?;
        let clip = compositor.CreateInsetClip()?;
        eyes.SetClip(&clip)?;
        blinker.Children()?.InsertAtTop(&eyes)?;

        let brush = compositor.CreateColorBrushWithColor(EYE)?;
        let rects = [compositor.CreateRoundedRectangleGeometry()?, compositor.CreateRoundedRectangleGeometry()?];
        let arcs = [compositor.CreateEllipseGeometry()?, compositor.CreateEllipseGeometry()?];
        let rect_shapes = [compositor.CreateSpriteShapeWithGeometry(&rects[0])?, compositor.CreateSpriteShapeWithGeometry(&rects[1])?];
        let arc_shapes = [compositor.CreateSpriteShapeWithGeometry(&arcs[0])?, compositor.CreateSpriteShapeWithGeometry(&arcs[1])?];
        for s in &rect_shapes {
            s.SetFillBrush(&brush)?;
        }
        for s in &arc_shapes {
            s.SetStrokeBrush(&brush)?;
            s.SetStrokeStartCap(CompositionStrokeCap::Round)?;
            s.SetStrokeEndCap(CompositionStrokeCap::Round)?;
        }
        // Two transparent white outlines (outer wide and dim, inner thin and brighter) use the same eye
        // geometry, so they blink and change shape along with it without extra animation.
        let halo_brushes = [
            compositor.CreateColorBrushWithColor(Color { A: 20, R: 255, G: 255, B: 255 })?,
            compositor.CreateColorBrushWithColor(Color { A: 46, R: 255, G: 255, B: 255 })?,
        ];
        let halo = |g: &CompositionGeometry, b: &CompositionColorBrush| -> Result<CompositionSpriteShape> {
            let s = compositor.CreateSpriteShapeWithGeometry(g)?;
            s.SetStrokeBrush(b)?;
            s.SetStrokeLineJoin(CompositionStrokeLineJoin::Round)?;
            s.SetStrokeStartCap(CompositionStrokeCap::Round)?;
            s.SetStrokeEndCap(CompositionStrokeCap::Round)?;
            Ok(s)
        };
        let mut rect_halos = Vec::new();
        let mut arc_halos = Vec::new();
        for i in 0..2 {
            for b in &halo_brushes {
                rect_halos.push(halo(&rects[i].cast()?, b)?);
                arc_halos.push(halo(&arcs[i].cast()?, b)?);
            }
        }

        Ok(Self {
            ease: compositor.CreateCubicBezierEasingFunction(Vector2 { X: 0.45, Y: 0.0 }, Vector2 { X: 0.55, Y: 1.0 })?,
            compositor: compositor.clone(),
            root,
            motion,
            wobble,
            blinker,
            eyes,
            rects,
            arcs,
            rect_shapes,
            arc_shapes,
            rect_halos,
            arc_halos,
            brush,
            clip,
            state: None,
            ticks: 0,
            rects_shown: false,
            place,
            placed: None,
            blink_at: std::cell::Cell::new(None),
            morph_at: std::cell::Cell::new(None),
        })
    }

    /// Shows Nochi with the face center `from_left` from the notch shape's left edge and `y` from the top
    /// (logical); `music`: a song is playing. Its position follows the shape's left edge every frame, so
    /// when the notch grows or shrinks Nochi isn't clipped outside the shape. If its place
    /// inside the shape changes (e.g. hovered Live), it shifts over `travel_ms`.
    #[allow(clippy::too_many_arguments)]
    pub fn show(&mut self, size: Size, mood: Mood, music: bool, from_left: f32, y: f32, scale: f32, travel_ms: u32) -> Result<()> {
        self.root.SetScale(Vector3 { X: scale, Y: scale, Z: 1.0 })?;
        let target = ((from_left - BOX_W / 2.0) * scale, (y - BOX_H / 2.0) * scale);
        if self.placed != Some(target) {
            for (key, value) in [(h!("r"), target.0), (h!("y"), target.1)] {
                if self.placed.is_some() && travel_ms > 0 {
                    let ease = self.compositor.CreateCubicBezierEasingFunction(Vector2 { X: 0.2, Y: 0.9 }, Vector2 { X: 0.3, Y: 1.0 })?;
                    let anim = self.compositor.CreateScalarKeyFrameAnimation()?;
                    anim.InsertKeyFrameWithEasingFunction(1.0, value, &ease)?;
                    anim.SetDuration(ticks(travel_ms))?;
                    self.place.StartAnimation(key, &anim)?;
                } else {
                    self.place.StopAnimation(key)?;
                    self.place.InsertScalar(key, value)?;
                }
            }
            self.placed = Some(target);
        }
        self.root.SetOpacity(1.0)?;
        if self.state == Some((size, mood, music)) {
            return Ok(());
        }
        // If Nochi is already shown, the eyes morph smoothly instead of jumping. If only the
        // music status changes, the face stays; only the repeating motion changes.
        let morph = self.state.is_some();
        let same_face = self.state.is_some_and(|(s, m, _)| s == size && m == mood);
        self.state = Some((size, mood, music));
        self.stop(morph)?;
        if !same_face {
            self.set_face(size, mood, morph)?;
        }
        match mood {
            Mood::Focused | Mood::Thinking if music => self.nod_loop(),
            Mood::Focused => self.read_loop(),
            Mood::Happy { hop: true } => self.hop_loop(),
            Mood::Alert => self.pulse_loop(),
            _ => Ok(()),
        }
    }

    pub fn hide(&mut self) -> Result<()> {
        self.placed = None;
        if self.state.take().is_some() {
            self.stop(false)?;
            self.root.SetOpacity(0.0)?;
        }
        Ok(())
    }

    /// Glances sideways (`x` logical, + = right) while being carried; 0 returns to center.
    pub fn look(&self, x: f32) -> Result<()> {
        let k = self.state.map_or(1.0, |(size, _, _)| size.scale());
        self.tween3(&self.eyes.cast()?, h!("Offset"), v3(x * k, 0.0))
    }

    /// The current mood has an idle motion (blink, glance) triggered by the app timer.
    pub fn wants_idle(&self) -> bool {
        matches!(self.state, Some((_, Mood::Idle | Mood::Thinking | Mood::Curious | Mood::Zen, _)))
    }

    /// Called by the app timer every few seconds: blink, and occasionally glance.
    /// Returns `true` if Nochi is in a mood that has an idle motion.
    pub fn idle_tick(&mut self) -> Result<bool> {
        if !self.wants_idle() {
            return Ok(false);
        }
        self.ticks = self.ticks.wrapping_add(1);
        if self.ticks.is_multiple_of(3) && matches!(self.state, Some((_, Mood::Idle, _))) {
            self.glance_once()?;
        } else {
            self.blink_once()?;
        }
        Ok(true)
    }

    /// Delay until the next idle motion; slightly random so it doesn't feel mechanical.
    pub fn next_tick_ms(&self) -> u32 {
        const GAPS: [u32; 5] = [3800, 5200, 4300, 6100, 2900];
        GAPS[self.ticks as usize % GAPS.len()]
    }

    fn morph_to(&self, to: Vector2) -> Result<Vector2KeyFrameAnimation> {
        let anim = self.compositor.CreateVector2KeyFrameAnimation()?;
        anim.InsertKeyFrameWithEasingFunction(1.0, to, &self.ease)?;
        anim.SetDuration(ticks(MORPH_MS))?;
        Ok(anim)
    }

    fn tween3(&self, target: &Visual, prop: &HSTRING, to: Vector3) -> Result<()> {
        let anim = self.compositor.CreateVector3KeyFrameAnimation()?;
        anim.InsertKeyFrameWithEasingFunction(1.0, to, &self.ease)?;
        anim.SetDuration(ticks(MORPH_MS))?;
        target.StartAnimation(prop, &anim)
    }

    fn set_face(&mut self, size: Size, mood: Mood, morph: bool) -> Result<()> {
        self.morph_at.set(Some(std::time::Instant::now()));
        if cfg!(debug_assertions) {
            eprintln!("[notch] pet face {size:?} {mood:?} morph={morph}");
        }
        let k = size.scale();
        let (eyes, half) = face(mood);
        let half = half * k;
        let (cx, cy) = (BOX_W / 2.0, BOX_H / 2.0);
        // Thinking: glances to the upper right. Curious: turns to look down, toward the cursor. The
        // thinking glance is kept small so the eyes stay inside the orb when Tiny is in the ring.
        let (dx, dy) = match mood {
            Mood::Thinking => (2.2 * k, -2.0 * k),
            Mood::Curious => (2.0 * k, 1.5 * k),
            Mood::Zen => (0.0, 1.5 * k),
            _ => (0.0, 0.0),
        };
        // The blink is centered on the glancing eye, not the middle of the box; otherwise the eye
        // would shift to the center on every blink.
        let pivot = v3(cx + dx, cy + dy);
        if morph {
            self.tween3(&self.eyes.cast()?, h!("Offset"), v3(dx, dy))?;
            self.tween3(&self.blinker.cast()?, h!("CenterPoint"), pivot)?;
        } else {
            self.eyes.StopAnimation(h!("Offset"))?;
            self.eyes.SetOffset(v3(dx, dy))?;
            self.blinker.StopAnimation(h!("CenterPoint"))?;
            self.blinker.SetCenterPoint(pivot)?;
        }
        let alpha = if mood == Mood::Sleepy { 190 } else { 255 };
        self.brush.SetColor(Color { A: alpha, ..EYE })?;
        let shapes = self.eyes.Shapes()?;
        match eyes {
            Eyes::Rect(w, h) => {
                let (w, h) = (w * k, h * k);
                let morph = morph && self.rects_shown;
                self.clip.SetBottomInset(0.0)?;
                if !morph {
                    shapes.Clear()?;
                }
                for (i, g) in self.rects.iter().enumerate() {
                    let x = if i == 0 { cx - half } else { cx + half };
                    let r = w.min(h) / 2.0;
                    let targets = [
                        ("Size", Vector2 { X: w, Y: h }),
                        ("Offset", Vector2 { X: x - w / 2.0, Y: cy - h / 2.0 }),
                        ("CornerRadius", Vector2 { X: r, Y: r }),
                    ];
                    for (prop, to) in targets {
                        let prop = HSTRING::from(prop);
                        if morph {
                            g.StartAnimation(&prop, &self.morph_to(to)?)?;
                        } else {
                            g.StopAnimation(&prop)?;
                        }
                    }
                    if !morph {
                        g.SetSize(targets[0].1)?;
                        g.SetOffset(targets[1].1)?;
                        g.SetCornerRadius(targets[2].1)?;
                        for h in &self.rect_halos[i * 2..i * 2 + 2] {
                            shapes.Append(h)?;
                        }
                        shapes.Append(&self.rect_shapes[i])?;
                    }
                }
                for (j, h) in self.rect_halos.iter().enumerate() {
                    h.SetStrokeThickness(if j % 2 == 0 { 6.0 * k } else { 3.0 * k })?;
                }
                self.rects_shown = true;
            }
            Eyes::Arc(r) => {
                // Top arc of the circle; the bottom half is cut off by the clip. The center is lowered so
                // the arc appears in the middle.
                let r = r * k;
                let arc_y = cy + r / 2.0;
                let line = (2.2 * k).max(1.4);
                for shape in &self.arc_shapes {
                    shape.SetStrokeThickness(line)?;
                }
                for (j, h) in self.arc_halos.iter().enumerate() {
                    h.SetStrokeThickness(line + if j % 2 == 0 { 5.0 * k } else { 2.5 * k })?;
                }
                self.clip.SetBottomInset(BOX_H - arc_y)?;
                shapes.Clear()?;
                self.rects_shown = false;
                for (i, g) in self.arcs.iter().enumerate() {
                    let x = if i == 0 { cx - half } else { cx + half };
                    g.SetCenter(Vector2 { X: x, Y: arc_y })?;
                    g.SetRadius(Vector2 { X: r, Y: r })?;
                    for h in &self.arc_halos[i * 2..i * 2 + 2] {
                        shapes.Append(h)?;
                    }
                    shapes.Append(&self.arc_shapes[i])?;
                }
            }
        }
        Ok(())
    }

    /// Stops repeating motion. With `smooth`, the eyes return to normal position and size
    /// via a short animation, instead of jumping.
    fn stop(&self, smooth: bool) -> Result<()> {
        // A stopped animation leaves its value at the last position.
        self.motion.StopAnimation(h!("Offset"))?;
        self.wobble.StopAnimation(h!("Offset"))?;
        self.blinker.StopAnimation(h!("Scale"))?;
        if smooth {
            // The remains of the motion are moved to `motion` then damped, so the new motion in `wobble`
            // can start from zero without making the eyes jump.
            let (m, w) = (self.motion.Offset()?, self.wobble.Offset()?);
            self.wobble.SetOffset(v3(0.0, 0.0))?;
            self.motion.SetOffset(v3(m.X + w.X, m.Y + w.Y))?;
            self.tween3(&self.motion.cast()?, h!("Offset"), v3(0.0, 0.0))?;
            return self.tween3(&self.blinker.cast()?, h!("Scale"), Vector3 { X: 1.0, Y: 1.0, Z: 1.0 });
        }
        self.motion.SetOffset(v3(0.0, 0.0))?;
        self.wobble.SetOffset(v3(0.0, 0.0))?;
        self.blinker.SetScale(Vector3 { X: 1.0, Y: 1.0, Z: 1.0 })
    }

    /// Motion steps are written for the Pill size and scaled to the current size, so that in
    /// the Tiny ring the eyes don't hit the orb's edge.
    fn offsets(&self, frames: &[(f32, f32, f32)], ms: u32, forever: bool) -> Result<()> {
        let k = self.state.map_or(1.0, |(size, _, _)| size.scale());
        let anim = self.compositor.CreateVector3KeyFrameAnimation()?;
        for &(t, x, y) in frames {
            anim.InsertKeyFrameWithEasingFunction(t, v3(x * k, y * k), &self.ease)?;
        }
        anim.SetDuration(ticks(ms))?;
        if forever {
            anim.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
        }
        self.wobble.StartAnimation(h!("Offset"), &anim)
    }

    fn scales(&self, frames: &[(f32, f32, f32)], ms: u32, forever: bool) -> Result<()> {
        let anim = self.compositor.CreateVector3KeyFrameAnimation()?;
        for &(t, x, y) in frames {
            anim.InsertKeyFrameWithEasingFunction(t, Vector3 { X: x, Y: y, Z: 1.0 }, &self.ease)?;
        }
        anim.SetDuration(ticks(ms))?;
        if forever {
            anim.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
        }
        self.blinker.StartAnimation(h!("Scale"), &anim)
    }

    /// Blink: closes slowly, stays closed briefly, then opens (`BLINK_MS`). For square eyes, the eye
    /// shape itself is animated (height, position, corner), not scaled: when closed the eye
    /// becomes a sharp 2 px line with still-round ends. Vertical scaling used to flatten the corners
    /// too and put the ±1.5 px line between two pixel rows, so it looked like it was flickering.
    pub fn blink_once(&self) -> Result<()> {
        if cfg!(debug_assertions) {
            eprintln!("[notch] pet blink");
        }
        let Some((size, mood, _)) = self.state else { return Ok(()) };
        let since = |c: &std::cell::Cell<Option<std::time::Instant>>| c.get().map_or(u128::MAX, |t| t.elapsed().as_millis());
        if since(&self.morph_at) < (MORPH_MS + 60) as u128 || since(&self.blink_at) < (BLINK_MS + 80) as u128 {
            return Ok(());
        }
        self.blink_at.set(Some(std::time::Instant::now()));
        let ease_in = self.compositor.CreateCubicBezierEasingFunction(Vector2 { X: 0.4, Y: 0.0 }, Vector2 { X: 1.0, Y: 1.0 })?;
        let ease_out = self.compositor.CreateCubicBezierEasingFunction(Vector2 { X: 0.0, Y: 0.0 }, Vector2 { X: 0.2, Y: 1.0 })?;
        let (eyes, half) = face(mood);
        let Eyes::Rect(w, h) = eyes else {
            // Arc eyes (^ ^): only a slight flattening is needed.
            let anim = self.compositor.CreateVector3KeyFrameAnimation()?;
            anim.InsertKeyFrameWithEasingFunction(0.3, Vector3 { X: 1.0, Y: 0.35, Z: 1.0 }, &ease_in)?;
            anim.InsertKeyFrame(0.55, Vector3 { X: 1.0, Y: 0.35, Z: 1.0 })?;
            anim.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: 1.0, Y: 1.0, Z: 1.0 }, &ease_out)?;
            anim.SetDuration(ticks(BLINK_MS))?;
            return self.blinker.StartAnimation(h!("Scale"), &anim);
        };
        let k = size.scale();
        let (w, h, half) = (w * k, h * k, half * k);
        let shut = h.min(2.0);
        let (cx, cy) = (BOX_W / 2.0, BOX_H / 2.0);
        for (i, g) in self.rects.iter().enumerate() {
            let x = if i == 0 { cx - half } else { cx + half };
            let open = [(w, h), (x - w / 2.0, cy - h / 2.0), (w.min(h) / 2.0, w.min(h) / 2.0)];
            let closed = [(w, shut), (x - w / 2.0, cy - shut / 2.0), (w.min(shut) / 2.0, w.min(shut) / 2.0)];
            for (prop, (o, c)) in ["Size", "Offset", "CornerRadius"].into_iter().zip(open.into_iter().zip(closed)) {
                let anim = self.compositor.CreateVector2KeyFrameAnimation()?;
                let (open, closed) = (Vector2 { X: o.0, Y: o.1 }, Vector2 { X: c.0, Y: c.1 });
                anim.InsertKeyFrameWithEasingFunction(0.3, closed, &ease_in)?;
                anim.InsertKeyFrame(0.55, closed)?;
                anim.InsertKeyFrameWithEasingFunction(1.0, open, &ease_out)?;
                anim.SetDuration(ticks(BLINK_MS))?;
                g.StartAnimation(&HSTRING::from(prop), &anim)?;
            }
        }
        Ok(())
    }

    fn glance_once(&self) -> Result<()> {
        self.offsets(
            &[(0.0, 0.0, 0.0), (0.15, -3.0, 0.0), (0.45, -3.0, 0.0), (0.6, 3.0, 0.0), (0.85, 3.0, 0.0), (1.0, 0.0, 0.0)],
            2600,
            false,
        )
    }

    fn read_loop(&self) -> Result<()> {
        // Starts and ends at the center, so entering this motion has no jump.
        self.offsets(&[(0.0, 0.0, 0.0), (0.25, -3.0, 0.0), (0.75, 3.0, 0.0), (1.0, 0.0, 0.0)], 2200, true)
    }

    /// Nods gently, roughly 100 bpm. The motion is small and damped so it stays calm
    /// inside the ring; starts and ends at the center, so transitions to and from other moods are smooth.
    fn nod_loop(&self) -> Result<()> {
        self.offsets(&[(0.0, 0.0, 0.0), (0.5, 0.0, 1.4), (1.0, 0.0, 0.0)], 600, true)
    }

    fn hop_loop(&self) -> Result<()> {
        self.offsets(&[(0.0, 0.0, 0.0), (0.45, 0.0, -3.0), (1.0, 0.0, 0.0)], 900, true)
    }

    fn pulse_loop(&self) -> Result<()> {
        self.scales(&[(0.0, 1.0, 1.0), (0.5, 1.12, 1.12), (1.0, 1.0, 1.0)], 1400, true)
    }
}
