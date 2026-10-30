//! Notch window: Win32 + Windows.UI.Composition.
//!
//! Visual tree (bottom to top): main shape (rounded rect attached to the top edge, with a thin
//! outline), two concave "shoulders" at the top left and right, content layer (with Nochi as its
//! child, so it follows the appear effect), then the chrome ring.
//! All animations run in DWM; this process only redraws content when state changes.
//! Mouse events are not handled here; everything is forwarded to the controller window.

use std::sync::atomic::{AtomicBool, Ordering};

use windows::{
    core::*,
    Foundation::{Size, TimeSpan},
    Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat},
    UI::Color,
    UI::Composition::Desktop::DesktopWindowTarget,
    UI::Composition::*,
    Win32::Foundation::*,
    Win32::Graphics::Direct2D::Common::*,
    Win32::Graphics::Direct2D::*,
    Win32::Graphics::Direct3D::*,
    Win32::Graphics::Direct3D11::*,
    Win32::Graphics::DirectWrite::*,
    Win32::Graphics::Dxgi::IDXGIDevice,
    Win32::Graphics::Gdi::{CreateRectRgn, DeleteObject, SetWindowRgn},
    Win32::System::WinRT::Composition::*,
    Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT},
    Win32::UI::WindowsAndMessaging::*,
};
use windows_numerics::{Matrix3x2, Vector2, Vector3};

use crate::app::{WM_APP_CLICK, WM_APP_HOVER_ENTER, WM_APP_HOVER_LEAVE, WM_APP_MOUSEMOVE, WM_APP_MOVE, WM_APP_RCLICK, WM_APP_RELAYOUT, WM_APP_WHEEL};
use crate::geometry::{Placement, Rect, Shape, SHOULDER};
use crate::paint::{
    self, layout::MAX_RINGS, Area, Content, Marquee, MoonSpot, Painted, Painter, PetSpot, RingSpot, MARQUEE_GAP, MOON_BOX, MOON_GLOW, MOON_ICON, RING_MAX,
};
use crate::pet::{Mood, Pet};
use crate::spring::Spring;
use crate::{theme, watch};

const CLASS: PCWSTR = w!("NotchIsland");
/// Equalizer bars (x from center, full height); slightly roomier than the `Waveform` icon so that
/// the motion reads well.
const EQ_BAR_W: f32 = 2.0;
const EQ_BARS: [(f32, f32); 4] = [(-4.5, 5.0), (-1.5, 11.0), (1.5, 7.5), (4.5, 4.2)];
/// Each bar has its own tempo and pattern so they don't look synchronized: (ms, [(time, height scale)]).
const EQ_MOTION: [(u32, [(f32, f32); 3]); 4] = [
    (760, [(0.0, 0.5), (0.5, 1.3), (1.0, 0.5)]),
    (920, [(0.0, 1.0), (0.45, 0.35), (1.0, 1.0)]),
    (680, [(0.0, 0.45), (0.55, 1.0), (1.0, 0.45)]),
    (840, [(0.0, 1.4), (0.5, 0.6), (1.0, 1.4)]),
];
/// Defined in the large `Win32_UI_Controls` feature; only the constant is needed.
const WM_MOUSELEAVE: u32 = 0x02A3;
const SPRING_STEPS: usize = 24;
/// Duration of one chrome ring revolution; slow, like a metallic glint shifting.
const RING_PERIOD_MS: u32 = 4000;
/// Crescent moon rocking like a cradle: (time, angle in degrees) over one full swing.
const MOON_ROCK_MS: u32 = 4600;
const MOON_ROCK: [(f32, f32); 5] = [(0.0, 0.0), (0.25, -9.0), (0.5, 0.0), (0.75, 7.0), (1.0, 0.0)];
/// Glow behind the moon breathes gently: (time, opacity, scale).
const MOON_GLOW_MS: u32 = 3200;
const MOON_BREATH: [(f32, f32, f32); 3] = [(0.0, 0.35, 0.86), (0.5, 1.0, 1.04), (1.0, 0.35, 0.86)];

static TRACKING: AtomicBool = AtomicBool::new(false);
/// Mouse movement is only forwarded while the panel is open (for the row hover effect).
static PANEL: AtomicBool = AtomicBool::new(false);

pub fn set_panel_mode(on: bool) {
    PANEL.store(on, Ordering::Relaxed);
}

/// The compact notch may be dragged to another monitor (not the panel or cards, whose contents are clicked).
static MOVABLE: AtomicBool = AtomicBool::new(false);
/// Left button pressed in the notch window (not elsewhere and then released here).
static DOWN: AtomicBool = AtomicBool::new(false);
static PRESSED: AtomicBool = AtomicBool::new(false);
static DRAGGING: AtomicBool = AtomicBool::new(false);
static PRESS_AT: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
/// Distance (pixels) before press-and-drag counts as dragging, not a click.
const DRAG_SLOP: i32 = 12;

/// WPARAM `WM_APP_MOVE`; LPARAM = cursor position on screen (x, y as i16).
pub const MOVE_START: usize = 1;
pub const MOVE_MOVE: usize = 2;
pub const MOVE_END: usize = 3;

pub fn set_movable(on: bool) {
    MOVABLE.store(on, Ordering::Relaxed);
}

fn cursor() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

fn pack((x, y): (i32, i32)) -> isize {
    ((y as i16 as u16 as isize) << 16) | x as i16 as u16 as isize
}

pub fn unpack(lp: isize) -> (i32, i32) {
    ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_MOUSEMOVE => {
                if !TRACKING.swap(true, Ordering::Relaxed) {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut tme);
                    watch::post(WM_APP_HOVER_ENTER, 0, 0);
                }
                if PANEL.load(Ordering::Relaxed) {
                    watch::post(WM_APP_MOUSEMOVE, 0, lp.0);
                }
                // Press then drag the notch: move monitors.
                if PRESSED.load(Ordering::Relaxed) && wp.0 & 0x0001 != 0 {
                    let now = cursor();
                    if !DRAGGING.load(Ordering::Relaxed) {
                        let (x0, y0) = unpack(PRESS_AT.load(Ordering::Relaxed));
                        if (now.0 - x0).abs() > DRAG_SLOP || (now.1 - y0).abs() > DRAG_SLOP {
                            DRAGGING.store(true, Ordering::Relaxed);
                            windows::Win32::UI::Input::KeyboardAndMouse::SetCapture(hwnd);
                            watch::post(WM_APP_MOVE, MOVE_START, pack(now));
                        }
                    }
                    if DRAGGING.load(Ordering::Relaxed) {
                        watch::post(WM_APP_MOVE, MOVE_MOVE, pack(now));
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                DOWN.store(true, Ordering::Relaxed);
                let movable = MOVABLE.load(Ordering::Relaxed);
                PRESSED.store(movable, Ordering::Relaxed);
                if movable {
                    PRESS_AT.store(pack(cursor()), Ordering::Relaxed);
                }
                LRESULT(0)
            }
            WM_CAPTURECHANGED => {
                if DRAGGING.swap(false, Ordering::Relaxed) {
                    PRESSED.store(false, Ordering::Relaxed);
                    watch::post(WM_APP_MOVE, MOVE_END, pack(cursor()));
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                TRACKING.store(false, Ordering::Relaxed);
                watch::post(WM_APP_HOVER_LEAVE, 0, 0);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                PRESSED.store(false, Ordering::Relaxed);
                // Click only if the button was also pressed on the notch: a mouse-up from a press that started
                // elsewhere (e.g. a card appears under the cursor) must not approve anything.
                let pressed_here = DOWN.swap(false, Ordering::Relaxed);
                if DRAGGING.swap(false, Ordering::Relaxed) {
                    let _ = windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
                    watch::post(WM_APP_MOVE, MOVE_END, pack(cursor()));
                } else if pressed_here {
                    watch::post(WM_APP_CLICK, 0, lp.0);
                }
                LRESULT(0)
            }
            WM_RBUTTONUP => {
                watch::post(WM_APP_RCLICK, 0, lp.0);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                watch::post(WM_APP_WHEEL, wp.0, 0);
                LRESULT(0)
            }
            WM_DPICHANGED => {
                // While dragged across another monitor, the new layout is computed after release.
                if !DRAGGING.load(Ordering::Relaxed) {
                    watch::post(WM_APP_RELAYOUT, 0, 0);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn ticks(ms: u32) -> TimeSpan {
    TimeSpan { Duration: ms as i64 * 10_000 }
}

fn surface_size(w: f32, h: f32) -> Size {
    Size { Width: w.ceil().max(1.0), Height: h.ceil().max(1.0) }
}

pub struct Island {
    pub hwnd: HWND,
    compositor: Compositor,
    /// Held to keep it alive; its offset is always zero because the window is always canvas-sized.
    _root: ContainerVisual,
    shape_visual: ShapeVisual,
    sprite: CompositionSpriteShape,
    geom: CompositionRoundedRectangleGeometry,
    shoulders: [SpriteVisual; 2],
    shoulder_surfaces: [CompositionDrawingSurface; 2],
    /// Shape + shoulders. Deliberately no shadow: a DropShadow on this layer is a canvas-sized
    /// rectangle, so the whole window would look black and the rounded corners would vanish.
    body: ContainerVisual,
    /// Chrome ring around Nochi; rotated by DWM while the agent works.
    /// One ring per working session; all share the same surface. `bool`: currently spinning.
    rings: Vec<(SpriteVisual, bool)>,
    ring_surface: CompositionDrawingSurface,
    /// Equalizer bars (Live and the panel's media row); their height is animated by DWM while a song plays and is visible.
    eq: ContainerVisual,
    eq_bars: Vec<ShapeVisual>,
    eq_playing: bool,
    /// Crescent moon (prayer reminder, prayer card): a container at the moon's center, holding the amber glow and
    /// the moon. Each is drawn once; the rocking and the glow's breathing are run by DWM, only
    /// while the moon is visible.
    moon: ContainerVisual,
    moon_glow: SpriteVisual,
    moon_glow_surface: CompositionDrawingSurface,
    moon_crescent: SpriteVisual,
    moon_surface: CompositionDrawingSurface,
    /// The moon currently shown; its animation is not restarted while it stays shown.
    moon_now: Option<MoonSpot>,
    content: SpriteVisual,
    /// Nochi layer above the content, clipped to the same shape but not part of the content's appear
    /// effect (fade): if it were, Nochi's eyes would vanish briefly on every view change.
    pet_layer: ContainerVisual,
    content_surface: CompositionDrawingSurface,
    /// Text that is too long: a container clipped to its text area, holding a sprite that DWM shifts.
    marquee: ContainerVisual,
    marquee_sprite: SpriteVisual,
    marquee_surface: CompositionDrawingSurface,
    /// Black gradient left and right of the marquee, so the text fades instead of being cut sharply.
    marquee_fades: [SpriteVisual; 2],
    /// Marquee currently running; its animation is not restarted while it stays the same.
    marquee_now: Option<Marquee>,
    /// Duration of the last shape animation (ms; 0 = no animation), so Nochi can follow along.
    shape_ms: std::cell::Cell<u32>,
    /// Width of the last shape (logical); its left edge is the reference for Nochi's position.
    shape_w: std::cell::Cell<f32>,
    /// While dragged: cursor x and window x at the start, and whether the notch is still reluctant.
    float_start: std::cell::Cell<(i32, i32)>,
    resisting: std::cell::Cell<bool>,
    pet: Pet,
    /// Must stay alive while the surface is in use.
    _graphics: CompositionGraphicsDevice,
    d2d: ID2D1Factory1,
    dwrite: IDWriteFactory,
    linear: LinearEasingFunction,
    _target: DesktopWindowTarget,
    placement: Placement,
    /// Visible area (window region), screen pixels.
    window: Rect,
    /// OS window position; always the canvas.
    placed: Rect,
    visible: bool,
}

impl Island {
    pub fn create(instance: HINSTANCE, placement: Placement) -> Result<Self> {
        unsafe {
            let wc = WNDCLASSW {
                hCursor: LoadCursorW(None, IDC_HAND)?,
                hInstance: instance,
                lpszClassName: CLASS,
                lpfnWndProc: Some(wndproc),
                ..Default::default()
            };
            RegisterClassW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                CLASS,
                w!("Notch"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance),
                None,
            )?;

            let compositor = Compositor::new()?;
            let target = compositor.cast::<ICompositorDesktopInterop>()?.CreateDesktopWindowTarget(hwnd, true)?;
            let root = compositor.CreateContainerVisual()?;
            target.SetRoot(&root)?;

            let geom = compositor.CreateRoundedRectangleGeometry()?;
            let sprite = compositor.CreateSpriteShapeWithGeometry(&geom)?;
            let fill = compositor.CreateColorBrushWithColor(Color { A: 255, R: 0, G: 0, B: 0 })?;
            sprite.SetFillBrush(&fill)?;
            let hairline = Color { A: theme::HAIRLINE_ALPHA, R: 255, G: 255, B: 255 };
            sprite.SetStrokeBrush(&compositor.CreateColorBrushWithColor(hairline)?)?;
            let shape_visual = compositor.CreateShapeVisual()?;
            shape_visual.Shapes()?.Append(&sprite)?;
            let body = compositor.CreateContainerVisual()?;
            body.Children()?.InsertAtTop(&shape_visual)?;
            root.Children()?.InsertAtTop(&body)?;

            // WARP (CPU rasterizer), not GPU: GPU drivers (e.g. NVIDIA) load tens of MB of DLLs and
            // commit memory to this process, even though only text and small icons are drawn, and only when
            // state changes; animations still run in DWM on the GPU. Draw time is comparable, and WARP
            // doesn't go away when the GPU driver is updated or reset.
            let mut d3d: Option<ID3D11Device> = None;
            let mut create = |driver| {
                D3D11CreateDevice(
                    None,
                    driver,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut d3d),
                    None,
                    None,
                )
            };
            create(D3D_DRIVER_TYPE_WARP).or_else(|_| create(D3D_DRIVER_TYPE_HARDWARE))?;
            let dxgi: IDXGIDevice = d3d.ok_or_else(|| Error::from(E_FAIL))?.cast()?;
            let d2d: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let graphics = compositor.cast::<ICompositorInterop>()?.CreateGraphicsDevice(&d2d.CreateDevice(&dxgi)?)?;
            let new_surface = || {
                graphics.CreateDrawingSurface(
                    surface_size(1.0, 1.0),
                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    DirectXAlphaMode::Premultiplied,
                )
            };

            let shoulder_surfaces = [new_surface()?, new_surface()?];
            let shoulders = [compositor.CreateSpriteVisual()?, compositor.CreateSpriteVisual()?];
            for (v, s) in shoulders.iter().zip(&shoulder_surfaces) {
                v.SetBrush(&compositor.CreateSurfaceBrushWithSurface(s)?)?;
                body.Children()?.InsertAtTop(v)?;
            }

            let content_surface = new_surface()?;
            let content = compositor.CreateSpriteVisual()?;
            content.SetBrush(&compositor.CreateSurfaceBrushWithSurface(&content_surface)?)?;
            // Content is clipped to the animating shape: when the notch grows, new content
            // (already drawn at the final size) isn't visible outside the black shape.
            content.SetClip(&compositor.CreateGeometricClipWithGeometry(&geom)?)?;
            root.Children()?.InsertAtTop(&content)?;
            let pet = Pet::create(&compositor, &geom)?;
            let pet_layer = compositor.CreateContainerVisual()?;
            pet_layer.SetClip(&compositor.CreateGeometricClipWithGeometry(&geom)?)?;
            pet_layer.Children()?.InsertAtTop(&pet.root)?;
            root.Children()?.InsertAtTop(&pet_layer)?;

            let marquee = compositor.CreateContainerVisual()?;
            marquee.SetClip(&compositor.CreateInsetClip()?)?;
            marquee.SetOpacity(0.0)?;
            let marquee_surface = new_surface()?;
            let marquee_sprite = compositor.CreateSpriteVisual()?;
            marquee_sprite.SetBrush(&compositor.CreateSurfaceBrushWithSurface(&marquee_surface)?)?;
            marquee.Children()?.InsertAtTop(&marquee_sprite)?;
            // The notch background is always black, so fading to black at the edges only needs the two gradients above the text.
            let fade = |solid_first: bool| -> Result<SpriteVisual> {
                let brush = compositor.CreateLinearGradientBrush()?;
                brush.SetStartPoint(Vector2 { X: 0.0, Y: 0.0 })?;
                brush.SetEndPoint(Vector2 { X: 1.0, Y: 0.0 })?;
                let (a, b) = if solid_first { (255, 0) } else { (0, 255) };
                let stops = brush.ColorStops()?;
                stops.Append(&compositor.CreateColorGradientStopWithOffsetAndColor(0.0, Color { A: a, R: 0, G: 0, B: 0 })?)?;
                stops.Append(&compositor.CreateColorGradientStopWithOffsetAndColor(1.0, Color { A: b, R: 0, G: 0, B: 0 })?)?;
                let v = compositor.CreateSpriteVisual()?;
                v.SetBrush(&brush)?;
                marquee.Children()?.InsertAtTop(&v)?;
                Ok(v)
            };
            let marquee_fades = [fade(true)?, fade(false)?];
            content.Children()?.InsertAtTop(&marquee)?;

            let eq = compositor.CreateContainerVisual()?;
            eq.SetOpacity(0.0)?;
            content.Children()?.InsertAtTop(&eq)?;
            let eq_brush = compositor.CreateColorBrushWithColor(Color { A: 255, R: 245, G: 245, B: 247 })?;
            let mut eq_bars = Vec::new();
            for (x, h) in EQ_BARS {
                let geom = compositor.CreateRoundedRectangleGeometry()?;
                geom.SetSize(Vector2 { X: EQ_BAR_W, Y: h })?;
                geom.SetCornerRadius(Vector2 { X: EQ_BAR_W / 2.0, Y: EQ_BAR_W / 2.0 })?;
                let shape = compositor.CreateSpriteShapeWithGeometry(&geom)?;
                shape.SetFillBrush(&eq_brush)?;
                let bar = compositor.CreateShapeVisual()?;
                bar.SetSize(Vector2 { X: EQ_BAR_W, Y: h })?;
                bar.SetOffset(Vector3 { X: x - EQ_BAR_W / 2.0, Y: -h / 2.0, Z: 0.0 })?;
                // Extends from the center, like the static equalizer icon.
                bar.SetCenterPoint(Vector3 { X: EQ_BAR_W / 2.0, Y: h / 2.0, Z: 0.0 })?;
                bar.Shapes()?.Append(&shape)?;
                eq.Children()?.InsertAtTop(&bar)?;
                eq_bars.push(bar);
            }

            let moon = compositor.CreateContainerVisual()?;
            moon.SetOpacity(0.0)?;
            content.Children()?.InsertAtTop(&moon)?;
            let moon_glow_surface = new_surface()?;
            let moon_glow = compositor.CreateSpriteVisual()?;
            moon_glow.SetBrush(&compositor.CreateSurfaceBrushWithSurface(&moon_glow_surface)?)?;
            moon_glow.SetOpacity(0.0)?;
            moon.Children()?.InsertAtTop(&moon_glow)?;
            let moon_surface = new_surface()?;
            let moon_crescent = compositor.CreateSpriteVisual()?;
            moon_crescent.SetBrush(&compositor.CreateSurfaceBrushWithSurface(&moon_surface)?)?;
            moon.Children()?.InsertAtTop(&moon_crescent)?;

            let ring_surface = new_surface()?;
            let mut rings = Vec::with_capacity(MAX_RINGS);
            for _ in 0..MAX_RINGS {
                let ring = compositor.CreateSpriteVisual()?;
                ring.SetBrush(&compositor.CreateSurfaceBrushWithSurface(&ring_surface)?)?;
                ring.SetOpacity(0.0)?;
                content.Children()?.InsertAtTop(&ring)?;
                rings.push((ring, false));
            }

            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let mut island = Self {
                hwnd,
                linear: compositor.CreateLinearEasingFunction()?,
                compositor,
                _root: root,
                shape_visual,
                sprite,
                geom,
                shoulders,
                shoulder_surfaces,
                body,
                rings,
                ring_surface,
                eq,
                eq_bars,
                eq_playing: false,
                moon,
                moon_glow,
                moon_glow_surface,
                moon_crescent,
                moon_surface,
                moon_now: None,
                content,
                pet_layer,
                content_surface,
                marquee,
                marquee_sprite,
                marquee_surface,
                marquee_fades,
                marquee_now: None,
                shape_ms: std::cell::Cell::new(0),
                shape_w: std::cell::Cell::new(0.0),
                float_start: std::cell::Cell::new((0, 0)),
                resisting: std::cell::Cell::new(false),
                pet,
                _graphics: graphics,
                d2d,
                dwrite,
                _target: target,
                placement,
                window: Rect::default(),
                placed: Rect::default(),
                visible: false,
            };
            island.set_placement(placement)?;
            Ok(island)
        }
    }

    pub fn window(&self) -> Rect {
        self.window
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Monitor move or DPI change: re-measure all layers and redraw the shoulders.
    pub fn set_placement(&mut self, placement: Placement) -> Result<()> {
        self.placement = placement;
        let s = placement.scale;
        let c = placement.canvas();
        let canvas = Vector2 { X: c.w as f32, Y: c.h as f32 };
        self.shape_visual.SetSize(canvas)?;
        self.body.SetSize(canvas)?;
        self.content.SetSize(canvas)?;
        self.pet_layer.SetSize(canvas)?;
        self.content.SetCenterPoint(Vector3 { X: c.w as f32 / 2.0, Y: 0.0, Z: 0.0 })?;
        self.content_surface.Resize(windows::Graphics::SizeInt32 { Width: c.w, Height: c.h })?;
        self.sprite.SetStrokeThickness(s)?;

        // Shoulders are radius + 1 pixel wide, covering the main shape's outline at the top.
        let (sw, sh) = ((SHOULDER + 1.0) * s, SHOULDER * s);
        for (i, v) in self.shoulders.iter().enumerate() {
            v.SetSize(Vector2 { X: sw, Y: sh })?;
            self.shoulder_surfaces[i].Resize(windows::Graphics::SizeInt32 { Width: sw.ceil() as i32, Height: sh.ceil() as i32 })?;
            // Shoulder position is bound to the geometry via ExpressionAnimation, so it follows every animation frame.
            let expr = if i == 0 { "Vector3(g.Offset.X - r, 0, 0)" } else { "Vector3(g.Offset.X + g.Size.X - o, 0, 0)" };
            let anim = self.compositor.CreateExpressionAnimationWithExpression(&HSTRING::from(expr))?;
            anim.SetReferenceParameter(h!("g"), &self.geom)?;
            anim.SetScalarParameter(h!("r"), SHOULDER * s)?;
            anim.SetScalarParameter(h!("o"), s)?;
            v.StartAnimation(h!("Offset"), &anim)?;
        }

        let rs = (RING_MAX * s).ceil();
        for (ring, _) in &self.rings {
            ring.SetSize(Vector2 { X: rs, Y: rs })?;
            ring.SetCenterPoint(Vector3 { X: rs / 2.0, Y: rs / 2.0, Z: 0.0 })?;
        }
        self.ring_surface.Resize(windows::Graphics::SizeInt32 { Width: rs as i32, Height: rs as i32 })?;
        // The moon and its glow are centered on their container's origin, so the container scales up from the center.
        for (sprite, surface, logical) in [(&self.moon_crescent, &self.moon_surface, MOON_BOX), (&self.moon_glow, &self.moon_glow_surface, MOON_GLOW)] {
            let px = (logical * s).ceil();
            sprite.SetSize(Vector2 { X: px, Y: px })?;
            sprite.SetOffset(Vector3 { X: -px / 2.0, Y: -px / 2.0, Z: 0.0 })?;
            sprite.SetCenterPoint(Vector3 { X: px / 2.0, Y: px / 2.0, Z: 0.0 })?;
            surface.Resize(windows::Graphics::SizeInt32 { Width: px as i32, Height: px as i32 })?;
        }
        self.with_surface(&self.moon_surface, |ctx| paint::draw_moon(ctx, &self.d2d, &self.dwrite))?;
        self.with_surface(&self.moon_glow_surface, |ctx| paint::draw_moon_glow(ctx, &self.d2d, &self.dwrite))?;
        self.draw_shoulders()?;
        // The marquee surface depends on scale; redrawn when reattached.
        self.set_marquee(None)?;
        self.draw_ring()
    }

    /// Runs text that is too long in its area, or stops it. The text is drawn once
    /// (two copies side by side) then shifted by DWM at a constant speed with a short pause at the start
    /// of each loop; no per-frame redraw.
    pub fn set_marquee(&mut self, m: Option<&Marquee>) -> Result<()> {
        const SPEED: f32 = 32.0; // px per second (logical)
        const PAUSE_MS: f32 = 1600.0;
        if self.marquee_now.as_ref() == m {
            return Ok(());
        }
        self.marquee_sprite.StopAnimation(h!("Offset"))?;
        self.marquee_fades[0].StopAnimation(h!("Opacity"))?;
        let Some(m) = m else {
            self.marquee.SetOpacity(0.0)?;
            self.marquee_now = None;
            return Ok(());
        };
        let s = self.placement.scale;
        let (w, h) = (m.area.r - m.area.l, m.area.b - m.area.t);
        self.marquee.SetOffset(Vector3 { X: m.area.l * s, Y: m.area.t * s, Z: 0.0 })?;
        self.marquee.SetSize(Vector2 { X: w * s, Y: h * s })?;
        // Left fade shows only while the text moves (see the animation below), so the first letter
        // is intact when idle at the start.
        for (v, (fw, x)) in self.marquee_fades.iter().zip([(12.0, 0.0), (18.0, w - 18.0)]) {
            v.SetSize(Vector2 { X: fw * s, Y: h * s })?;
            v.SetOffset(Vector3 { X: x * s, Y: 0.0, Z: 0.0 })?;
        }
        let shift = m.width + MARQUEE_GAP;
        let (sw, sh) = (((shift + m.width) * s).ceil(), (h * s).ceil());
        self.marquee_surface.Resize(windows::Graphics::SizeInt32 { Width: sw as i32, Height: sh as i32 })?;
        self.marquee_sprite.SetSize(Vector2 { X: sw, Y: sh })?;
        self.marquee_sprite.SetOffset(Vector3 { X: 0.0, Y: 0.0, Z: 0.0 })?;
        self.with_surface(&self.marquee_surface, |ctx| paint::draw_marquee(ctx, &self.d2d, &self.dwrite, m))?;

        let travel_ms = shift / SPEED * 1000.0;
        let total = PAUSE_MS + travel_ms;
        let anim = self.compositor.CreateVector3KeyFrameAnimation()?;
        anim.InsertKeyFrameWithEasingFunction(0.0, Vector3 { X: 0.0, Y: 0.0, Z: 0.0 }, &self.linear)?;
        anim.InsertKeyFrameWithEasingFunction(PAUSE_MS / total, Vector3 { X: 0.0, Y: 0.0, Z: 0.0 }, &self.linear)?;
        anim.InsertKeyFrameWithEasingFunction(1.0, Vector3 { X: -shift * s, Y: 0.0, Z: 0.0 }, &self.linear)?;
        anim.SetDuration(ticks(total as u32))?;
        anim.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
        self.marquee_sprite.StartAnimation(h!("Offset"), &anim)?;
        // Left fade: appears as soon as the text starts moving, disappears right before the next copy
        // stops at the start.
        const EDGE_MS: f32 = 180.0;
        let edge = self.compositor.CreateScalarKeyFrameAnimation()?;
        edge.InsertKeyFrameWithEasingFunction(0.0, 0.0, &self.linear)?;
        edge.InsertKeyFrameWithEasingFunction(PAUSE_MS / total, 0.0, &self.linear)?;
        edge.InsertKeyFrameWithEasingFunction((PAUSE_MS + EDGE_MS) / total, 1.0, &self.linear)?;
        edge.InsertKeyFrameWithEasingFunction((total - EDGE_MS) / total, 1.0, &self.linear)?;
        edge.InsertKeyFrameWithEasingFunction(1.0, 0.0, &self.linear)?;
        edge.SetDuration(ticks(total as u32))?;
        edge.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
        self.marquee_fades[0].StartAnimation(h!("Opacity"), &edge)?;
        self.marquee.SetOpacity(1.0)?;
        self.marquee_now = Some(m.clone());
        Ok(())
    }

    fn draw_ring(&self) -> Result<()> {
        let c = RING_MAX / 2.0;
        self.with_surface(&self.ring_surface, |ctx| paint::chrome_ring(ctx, &self.d2d, c, c, c - 0.8, 1.6, 1.0))
    }

    /// Shows a spinning ring around Nochi for each of `spots` (logical canvas coordinates);
    /// unused sprites are hidden. The sprite is drawn once at `RING_MAX` size then
    /// scaled. Rotation only runs while the ring is visible, so DWM doesn't redraw
    /// needlessly.
    pub fn set_rings(&mut self, spots: &[RingSpot]) -> Result<()> {
        let s = self.placement.scale;
        let half = (RING_MAX * s).ceil() / 2.0;
        for (i, (ring, spinning)) in self.rings.iter_mut().enumerate() {
            match spots.get(i) {
                Some(RingSpot { center: (x, y), diameter }) => {
                    ring.SetOffset(Vector3 { X: x * s - half, Y: y * s - half, Z: 0.0 })?;
                    let k = diameter / RING_MAX;
                    ring.SetScale(Vector3 { X: k, Y: k, Z: 1.0 })?;
                    if !*spinning {
                        let spin = self.compositor.CreateScalarKeyFrameAnimation()?;
                        spin.InsertKeyFrameWithEasingFunction(0.0, 0.0, &self.linear)?;
                        spin.InsertKeyFrameWithEasingFunction(1.0, 360.0, &self.linear)?;
                        spin.SetDuration(ticks(RING_PERIOD_MS))?;
                        spin.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
                        ring.StartAnimation(h!("RotationAngleInDegrees"), &spin)?;
                        let fade = self.compositor.CreateScalarKeyFrameAnimation()?;
                        fade.InsertKeyFrame(1.0, 1.0)?;
                        fade.SetDelayTime(ticks(150))?;
                        fade.SetDuration(ticks(250))?;
                        ring.StartAnimation(h!("Opacity"), &fade)?;
                        *spinning = true;
                    }
                }
                None if *spinning => {
                    ring.StopAnimation(h!("RotationAngleInDegrees"))?;
                    ring.StopAnimation(h!("Opacity"))?;
                    ring.SetOpacity(0.0)?;
                    *spinning = false;
                }
                None => {}
            }
        }
        Ok(())
    }

    /// Shows an animated equalizer centered at `center` (logical canvas), or hides it.
    /// Like the ring, its animation only runs while visible.
    pub fn set_eq(&mut self, center: Option<(f32, f32)>) -> Result<()> {
        match center {
            Some((x, y)) => {
                let s = self.placement.scale;
                self.eq.SetScale(Vector3 { X: s, Y: s, Z: 1.0 })?;
                self.eq.SetOffset(Vector3 { X: x * s, Y: y * s, Z: 0.0 })?;
                if !self.eq_playing {
                    let ease = self.compositor.CreateCubicBezierEasingFunction(Vector2 { X: 0.45, Y: 0.0 }, Vector2 { X: 0.55, Y: 1.0 })?;
                    for (bar, (ms, frames)) in self.eq_bars.iter().zip(EQ_MOTION) {
                        let anim = self.compositor.CreateVector3KeyFrameAnimation()?;
                        for (t, k) in frames {
                            anim.InsertKeyFrameWithEasingFunction(t, Vector3 { X: 1.0, Y: k, Z: 1.0 }, &ease)?;
                        }
                        anim.SetDuration(ticks(ms))?;
                        anim.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
                        bar.StartAnimation(h!("Scale"), &anim)?;
                    }
                    let fade = self.compositor.CreateScalarKeyFrameAnimation()?;
                    fade.InsertKeyFrame(0.0, 0.0)?;
                    fade.InsertKeyFrameWithEasingFunction(1.0, 1.0, &ease)?;
                    fade.SetDuration(ticks(260))?;
                    self.eq.StartAnimation(h!("Opacity"), &fade)?;
                    self.eq_playing = true;
                }
            }
            None if self.eq_playing => {
                for bar in &self.eq_bars {
                    bar.StopAnimation(h!("Scale"))?;
                }
                self.eq.StopAnimation(h!("Opacity"))?;
                self.eq.SetOpacity(0.0)?;
                self.eq_playing = false;
            }
            None => {}
        }
        Ok(())
    }

    /// Shows the rocking crescent moon at `spot` (canvas logical), with the amber glow
    /// breathing if `glow`, or hides it. Like the equalizer, its animation only runs
    /// while visible and is not restarted when the content is redrawn.
    pub fn set_moon(&mut self, spot: Option<MoonSpot>) -> Result<()> {
        let Some(m) = spot else {
            if self.moon_now.take().is_some() {
                self.moon_crescent.StopAnimation(h!("RotationAngleInDegrees"))?;
                self.moon_glow.StopAnimation(h!("Opacity"))?;
                self.moon_glow.StopAnimation(h!("Scale"))?;
                self.moon_glow.SetOpacity(0.0)?;
                self.moon.SetOpacity(0.0)?;
            }
            return Ok(());
        };
        let s = self.placement.scale;
        let k = m.size / MOON_ICON;
        self.moon.SetOffset(Vector3 { X: m.center.0 * s, Y: m.center.1 * s, Z: 0.0 })?;
        self.moon.SetScale(Vector3 { X: k, Y: k, Z: 1.0 })?;
        let before = self.moon_now.replace(m);
        let ease = || self.compositor.CreateCubicBezierEasingFunction(Vector2 { X: 0.45, Y: 0.0 }, Vector2 { X: 0.55, Y: 1.0 });
        if before.is_none() {
            let rock = self.compositor.CreateScalarKeyFrameAnimation()?;
            let ease = ease()?;
            for (t, deg) in MOON_ROCK {
                rock.InsertKeyFrameWithEasingFunction(t, deg, &ease)?;
            }
            rock.SetDuration(ticks(MOON_ROCK_MS))?;
            rock.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
            self.moon_crescent.StartAnimation(h!("RotationAngleInDegrees"), &rock)?;
            self.moon.SetOpacity(1.0)?;
        }
        if before.map(|b| b.glow) != Some(m.glow) {
            if m.glow {
                let ease = ease()?;
                let opacity = self.compositor.CreateScalarKeyFrameAnimation()?;
                let scale = self.compositor.CreateVector3KeyFrameAnimation()?;
                for (t, a, k) in MOON_BREATH {
                    opacity.InsertKeyFrameWithEasingFunction(t, a, &ease)?;
                    scale.InsertKeyFrameWithEasingFunction(t, Vector3 { X: k, Y: k, Z: 1.0 }, &ease)?;
                }
                opacity.SetDuration(ticks(MOON_GLOW_MS))?;
                opacity.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
                scale.SetDuration(ticks(MOON_GLOW_MS))?;
                scale.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
                self.moon_glow.StartAnimation(h!("Opacity"), &opacity)?;
                self.moon_glow.StartAnimation(h!("Scale"), &scale)?;
            } else {
                self.moon_glow.StopAnimation(h!("Opacity"))?;
                self.moon_glow.StopAnimation(h!("Scale"))?;
                self.moon_glow.SetOpacity(0.0)?;
            }
        }
        Ok(())
    }

    /// Shows Nochi where the painter decides, or hides it.
    pub fn set_pet(&mut self, spot: Option<PetSpot>, mood: Mood, music: bool) -> Result<()> {
        match spot {
            Some(spot) => {
                let s = self.placement.scale;
                let left = (self.placement.canvas().w as f32 / s - self.shape_w.get()) / 2.0;
                self.pet.show(spot.size, mood, music, spot.center.0 - left, spot.center.1, s, self.shape_ms.get())
            }
            None => self.pet.hide(),
        }
    }

    /// Nochi's idle motion (blink, glance). Returns the delay until the next motion, or `None`
    /// if the current mood needs no timer.
    pub fn pet_tick(&mut self) -> Result<Option<u32>> {
        Ok(self.pet.idle_tick()?.then(|| self.pet.next_tick_ms()))
    }

    /// Berkedip sekarang (perintah debug `--blink`).
    pub fn pet_blink(&self) -> Result<()> {
        self.pet.blink_once()
    }

    /// Nochi is shown in a mood that needs the idle motion timer.
    pub fn pet_wants_idle(&self) -> bool {
        self.pet.wants_idle()
    }

    fn draw_shoulders(&self) -> Result<()> {
        let r = SHOULDER;
        for (i, surface) in self.shoulder_surfaces.iter().enumerate() {
            let left = i == 0;
            self.with_surface(surface, |ctx| unsafe {
                let black = ctx.CreateSolidColorBrush(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 1.0 }, None)?;
                let line = ctx.CreateSolidColorBrush(
                    &D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: theme::HAIRLINE_ALPHA as f32 / 255.0 },
                    None,
                )?;
                // Left: box [0,r] minus a quarter circle centered at (0, r), plus a 1px column at x=r.
                // Right: its mirror image, with a 1px column at x=0.
                let (x0, cover) = if left { (0.0, (r, r + 1.0)) } else { (1.0, (0.0, 1.0)) };
                let p = |x: f32, y: f32| Vector2 { X: x0 + if left { x } else { r - x }, Y: y };
                let sweep = if left { D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE } else { D2D1_SWEEP_DIRECTION_CLOCKWISE };
                let arc = D2D1_ARC_SEGMENT {
                    point: p(0.0, 0.0),
                    size: D2D_SIZE_F { width: r, height: r },
                    rotationAngle: 0.0,
                    sweepDirection: sweep,
                    arcSize: D2D1_ARC_SIZE_SMALL,
                };

                let fill = self.d2d.CreatePathGeometry()?;
                let sink = fill.Open()?;
                sink.BeginFigure(p(0.0, 0.0), D2D1_FIGURE_BEGIN_FILLED);
                sink.AddLine(p(r, 0.0));
                sink.AddLine(p(r, r));
                sink.AddArc(&arc);
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                sink.Close()?;
                ctx.FillGeometry(&fill, &black, None);
                ctx.FillRectangle(&D2D_RECT_F { left: cover.0, top: 0.0, right: cover.1, bottom: r }, &black);

                let edge = self.d2d.CreatePathGeometry()?;
                let sink = edge.Open()?;
                sink.BeginFigure(p(r, r), D2D1_FIGURE_BEGIN_HOLLOW);
                sink.AddArc(&arc);
                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                sink.Close()?;
                ctx.DrawGeometry(&edge, &line, 1.0, None);
                Ok(())
            })?;
        }
        Ok(())
    }

    /// BeginDraw/EndDraw with a DPI scale transform, so `f` draws in logical pixels.
    fn with_surface(&self, surface: &CompositionDrawingSurface, f: impl FnOnce(&ID2D1DeviceContext) -> Result<()>) -> Result<()> {
        unsafe {
            let interop: ICompositionDrawingSurfaceInterop = surface.cast()?;
            let mut off = POINT::default();
            let ctx: ID2D1DeviceContext = interop.BeginDraw(None, &mut off)?;
            let s = self.placement.scale;
            ctx.SetTransform(&(Matrix3x2::scale(s, s) * Matrix3x2::translation(off.x as f32, off.y as f32)));
            ctx.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
            let result = f(&ctx);
            interop.EndDraw()?;
            result
        }
    }

    /// Changes the shape. With `spring`, the animation runs in DWM; returns its duration (ms).
    pub fn set_shape(&self, shape: &Shape, spring: Option<Spring>) -> Result<u32> {
        let s = self.placement.scale;
        let canvas_w = self.placement.canvas().w as f32;
        let (w, h, r) = (shape.w * s, shape.h * s, shape.r * s);
        self.shape_w.set(shape.w);
        // Shifted up by the radius, so only the bottom corners are rounded.
        let targets = [
            ("Size", Vector2 { X: w, Y: h + r }),
            ("Offset", Vector2 { X: (canvas_w - w) / 2.0, Y: -r }),
            ("CornerRadius", Vector2 { X: r, Y: r }),
        ];
        // Shoulders don't suit the thin line (lip).
        let shoulder_opacity = if shape.is_solid() { 1.0 } else { 0.0 };

        let Some(spring) = spring else {
            for (prop, _) in &targets {
                self.geom.StopAnimation(&HSTRING::from(*prop))?;
            }
            self.geom.SetSize(targets[0].1)?;
            self.geom.SetOffset(targets[1].1)?;
            self.geom.SetCornerRadius(targets[2].1)?;
            for v in &self.shoulders {
                v.SetOpacity(shoulder_opacity)?;
            }
            self.shape_ms.set(0);
            return Ok(0);
        };

        let duration = spring.duration_ms();
        self.shape_ms.set(duration);
        let frames = spring.keyframes(SPRING_STEPS);
        for (prop, value) in targets {
            let anim = self.compositor.CreateVector2KeyFrameAnimation()?;
            for (progress, k) in &frames {
                let expr = format!(
                    "this.StartingValue + (Vector2({:.3}, {:.3}) - this.StartingValue) * {:.5}",
                    value.X, value.Y, k
                );
                anim.InsertExpressionKeyFrameWithEasingFunction(*progress, &HSTRING::from(expr), &self.linear)?;
            }
            anim.SetDuration(ticks(duration))?;
            self.geom.StartAnimation(&HSTRING::from(prop), &anim)?;
        }
        for v in &self.shoulders {
            let fade = self.compositor.CreateScalarKeyFrameAnimation()?;
            fade.InsertKeyFrame(1.0, shoulder_opacity)?;
            fade.SetDuration(ticks(140))?;
            v.StartAnimation(h!("Opacity"), &fade)?;
        }
        Ok(duration)
    }

    /// Limits the visible window area and captures clicks to `r` (screen pixels). The OS window is always
    /// canvas-sized and never moved: moving the window and the content offset together doesn't always
    /// appear in the same frame, so the content could jump. DWM applies the window region atomically.
    pub fn set_window(&mut self, r: Rect) -> Result<()> {
        let c = self.placement.canvas();
        if self.placed != c {
            // Z-order is not changed here: the input window must stay above the notch. See `reassert_topmost`.
            unsafe { SetWindowPos(self.hwnd, None, c.x, c.y, c.w, c.h, SWP_NOACTIVATE | SWP_NOZORDER)? };
            self.placed = c;
        }
        unsafe {
            let region = CreateRectRgn(r.x - c.x, r.y - c.y, r.right() - c.x, r.bottom() - c.y);
            // On success the region is owned by the system; on failure, we free it.
            if SetWindowRgn(self.hwnd, Some(region), true) == 0 {
                let _ = DeleteObject(region.into());
            }
        }
        self.window = r;
        watch::set_island_rect(r);
        Ok(())
    }

    pub fn show(&mut self) {
        if !self.visible {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            }
            self.visible = true;
        }
    }

    pub fn hide(&mut self) {
        if self.visible {
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            self.visible = false;
        }
    }

    /// The Start menu and other topmost apps can cover the notch; called when the active window changes.
    pub fn reassert_topmost(&self) {
        if self.visible {
            unsafe {
                let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
    }

    /// New content appears with a fade, drops slightly, and grows from 96%.
    pub fn reveal(&self) -> Result<()> {
        let ease = self
            .compositor
            .CreateCubicBezierEasingFunction(Vector2 { X: 0.2, Y: 0.9 }, Vector2 { X: 0.3, Y: 1.0 })?;
        let delay = ticks(70);

        let opacity = self.compositor.CreateScalarKeyFrameAnimation()?;
        opacity.InsertKeyFrame(0.0, 0.0)?;
        opacity.InsertKeyFrameWithEasingFunction(1.0, 1.0, &ease)?;
        opacity.SetDuration(ticks(220))?;
        opacity.SetDelayTime(delay)?;
        opacity.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
        self.content.StartAnimation(h!("Opacity"), &opacity)?;

        for (prop, from) in [("Scale", Vector3 { X: 0.96, Y: 0.96, Z: 1.0 }), ("Offset", Vector3 { X: 0.0, Y: -5.0, Z: 0.0 })] {
            let to = if prop == "Scale" { Vector3 { X: 1.0, Y: 1.0, Z: 1.0 } } else { Vector3 { X: 0.0, Y: 0.0, Z: 0.0 } };
            let anim = self.compositor.CreateVector3KeyFrameAnimation()?;
            anim.InsertKeyFrame(0.0, from)?;
            anim.InsertKeyFrameWithEasingFunction(1.0, to, &ease)?;
            anim.SetDuration(ticks(320))?;
            anim.SetDelayTime(delay)?;
            anim.SetDelayBehavior(AnimationDelayBehavior::SetInitialValueBeforeDelay)?;
            self.content.StartAnimation(&HSTRING::from(prop), &anim)?;
        }
        Ok(())
    }

    // ---- Dragging between monitors --------------------------------------------------------------

    /// Start of drag. The notch stays attached to the top edge and only shifts sideways; at first it
    /// is reluctant (lags far behind the cursor while shaking its head), and once dragged far enough it
    /// follows.
    pub fn begin_float(&self, cursor: (i32, i32)) -> Result<()> {
        let w = self.window_pos();
        self.float_start.set((cursor.0, w.0));
        self.resisting.set(true);
        // While dragged the whole canvas may be visible, so the head shake isn't clipped by the shape bounds.
        // The region is restored when the layout is applied again.
        unsafe {
            SetWindowRgn(self.hwnd, None, true);
        }
        self.shake(true)
    }

    /// Follows the cursor along the top edge (`top`: top edge of the monitor under the cursor). For the first
    /// `SLACK` the notch only moves 30% of the cursor movement; after that it follows fully. Returns
    /// `true` while still reluctant.
    pub fn float_to(&self, cursor: (i32, i32), top: i32) -> Result<bool> {
        const SLACK: f32 = 180.0;
        let (cx0, wx0) = self.float_start.get();
        let slack = SLACK * self.placement.scale;
        let d = (cursor.0 - cx0) as f32;
        let disp = d.signum() * (0.3 * d.abs().min(slack) + (d.abs() - slack).max(0.0));
        self.move_window(wx0 + disp.round() as i32, top);
        let resisting = d.abs() < slack;
        if resisting != self.resisting.replace(resisting) {
            self.shake(resisting)?;
        }
        // Once it gives up, Nochi glances in the direction it's being taken.
        self.pet.look(if resisting { 0.0 } else { d.signum() * 3.0 })?;
        Ok(resisting)
    }

    /// Shakes its head "no": a small sideways shake, repeating with pauses.
    fn shake(&self, on: bool) -> Result<()> {
        self._root.StopAnimation(h!("Offset"))?;
        self._root.SetOffset(Vector3 { X: 0.0, Y: 0.0, Z: 0.0 })?;
        if !on {
            return Ok(());
        }
        let s = self.placement.scale;
        let anim = self.compositor.CreateVector3KeyFrameAnimation()?;
        for (t, x) in [(0.0, 0.0), (0.08, -4.0), (0.18, 4.0), (0.28, -3.0), (0.38, 2.0), (0.46, 0.0), (1.0, 0.0)] {
            anim.InsertKeyFrame(t, Vector3 { X: x * s, Y: 0.0, Z: 0.0 })?;
        }
        anim.SetDuration(ticks(1100))?;
        anim.SetIterationBehavior(AnimationIterationBehavior::Forever)?;
        self._root.StartAnimation(h!("Offset"), &anim)
    }

    /// Drag finished: stops shaking and looks straight ahead again.
    pub fn end_float(&self) -> Result<()> {
        self.resisting.set(false);
        self.shake(false)?;
        self.pet.look(0.0)
    }

    /// Top-left position of the window (screen pixels).
    pub fn window_pos(&self) -> (i32, i32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(self.hwnd, &mut r);
        }
        (r.left, r.top)
    }

    /// Moves the window without changing size, region, or z-order.
    pub fn move_window(&self, x: i32, y: i32) {
        unsafe {
            let _ = SetWindowPos(self.hwnd, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }

    /// The destination canvas has already been reached via `move_window`; `set_window` must not move it again.
    pub fn settle_at(&mut self, canvas: Rect) {
        self.placed = canvas;
    }

    /// Width of the glance shape (logical pixels) for a given text.
    pub fn glance_width(&self, text: &str) -> f32 {
        paint::glance_text_width(&self.dwrite, text) + paint::GLANCE_CHROME_W
    }

    /// Height of the card (permission, question, plan) that holds all its text, logical pixels.
    pub fn card_height(&self, content: &Content) -> f32 {
        paint::card_height(&self.dwrite, content)
    }

    pub fn draw(&self, content: &Content, shape: &Shape) -> Result<Painted> {
        let canvas_w = self.placement.canvas().w as f32 / self.placement.scale;
        let mut painted = Painted::default();
        self.with_surface(&self.content_surface, |ctx| {
            painted = Painter { ctx, d2d: &self.d2d, dwrite: &self.dwrite }.paint(content, shape, canvas_w)?;
            Ok(())
        })?;
        Ok(painted)
    }

    pub fn scale(&self) -> f32 {
        self.placement.scale
    }

    /// Window client coordinates (from the mouse message's LPARAM) to logical canvas coordinates. The window is always
    /// canvas-sized, so the two differ only by DPI scale.
    pub fn to_canvas(&self, lp: isize) -> (f32, f32) {
        let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
        let s = self.placement.scale;
        (x as f32 / s, y as f32 / s)
    }

    /// Logical canvas area to screen pixels, for placing the input window exactly on top of it.
    pub fn to_screen(&self, a: Area) -> Rect {
        let c = self.placement.canvas();
        let s = self.placement.scale;
        let (x, y) = ((a.l * s).round() as i32, (a.t * s).round() as i32);
        Rect { x: c.x + x, y: c.y + y, w: (a.r * s).round() as i32 - x, h: (a.b * s).round() as i32 - y }
    }
}
