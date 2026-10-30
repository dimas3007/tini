//! Spike: notch pill with Win32 + Windows.UI.Composition + Direct2D/DirectWrite.
//!
//! Left click: expand/collapse. Right click: quit.
//! `--cycle <ms>`: automatic toggle (to measure CPU during animation).

#![windows_subsystem = "windows"]

use std::cell::RefCell;

use windows::{
    core::*,
    Foundation::{Size, TimeSpan},
    Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat},
    System::DispatcherQueueController,
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
    Win32::System::LibraryLoader::GetModuleHandleW,
    Win32::System::WinRT::Composition::*,
    Win32::System::WinRT::*,
    Win32::UI::HiDpi::*,
    Win32::UI::WindowsAndMessaging::*,
};
use windows_numerics::{Matrix3x2, Vector2, Vector3};

/// (width, height, radius) in logical pixels, identical to the Slint variant.
const COLLAPSED: (f32, f32, f32) = (190.0, 34.0, 17.0);
const EXPANDED: (f32, f32, f32) = (680.0, 236.0, 28.0);
const ANIM_MS: i64 = 480;
const ID_SHRINK: usize = 1;
const ID_CYCLE: usize = 2;

struct App {
    hwnd: HWND,
    scale: f32,
    expanded: bool,
    /// Window area when expanded, in physical screen pixels.
    max_rect: RECT,
    compositor: Compositor,
    root: ContainerVisual,
    geom: CompositionRoundedRectangleGeometry,
    text_visual: SpriteVisual,
    surface: CompositionDrawingSurface,
    dwrite: IDWriteFactory,
    _target: DesktopWindowTarget,
    _graphics: CompositionGraphicsDevice,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn with_app(f: impl FnOnce(&mut App) -> Result<()>) {
    APP.with(|a| {
        if let Some(app) = a.borrow_mut().as_mut() {
            let _ = f(app);
        }
    });
}

fn ticks(ms: i64) -> TimeSpan {
    TimeSpan { Duration: ms * 10_000 }
}

fn color(r: f32, g: f32, b: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a: 1.0 }
}

fn rect(l: f32, t: f32, r: f32, b: f32) -> D2D_RECT_F {
    D2D_RECT_F { left: l, top: t, right: r, bottom: b }
}

impl App {
    fn dims(&self, expanded: bool) -> (f32, f32, f32) {
        let (w, h, r) = if expanded { EXPANDED } else { COLLAPSED };
        (w * self.scale, h * self.scale, r * self.scale)
    }

    fn max_w(&self) -> f32 {
        (self.max_rect.right - self.max_rect.left) as f32
    }

    /// Pill shape: a rounded rect shifted up by the radius, so only the bottom corners are rounded.
    fn apply_shape(&self, expanded: bool, animate: bool) -> Result<()> {
        let (w, h, r) = self.dims(expanded);
        let size = Vector2 { X: w, Y: h + r };
        let offset = Vector2 { X: (self.max_w() - w) / 2.0, Y: -r };
        let radius = Vector2 { X: r, Y: r };
        if !animate {
            self.geom.SetSize(size)?;
            self.geom.SetOffset(offset)?;
            return self.geom.SetCornerRadius(radius);
        }
        // The same overshoot curve as the Slint variant. The animation is run by DWM, not this process.
        let easing = self.compositor.CreateCubicBezierEasingFunction(
            Vector2 { X: 0.32, Y: 1.25 },
            Vector2 { X: 0.5, Y: 1.0 },
        )?;
        for (prop, value) in [("Size", size), ("Offset", offset), ("CornerRadius", radius)] {
            let anim = self.compositor.CreateVector2KeyFrameAnimation()?;
            anim.InsertExpressionKeyFrame(0.0, h!("this.StartingValue"))?;
            anim.InsertKeyFrameWithEasingFunction(1.0, value, &easing)?;
            anim.SetDuration(ticks(ANIM_MS))?;
            self.geom.StartAnimation(&HSTRING::from(prop), &anim)?;
        }
        Ok(())
    }

    fn fade_in_text(&self) -> Result<()> {
        self.text_visual.SetOpacity(0.0)?;
        let anim = self.compositor.CreateScalarKeyFrameAnimation()?;
        anim.InsertKeyFrame(0.0, 0.0)?;
        anim.InsertKeyFrame(1.0, 1.0)?;
        anim.SetDelayTime(ticks(120))?;
        anim.SetDuration(ticks(220))?;
        self.text_visual.StartAnimation(h!("Opacity"), &anim)
    }

    fn draw_text(&self, expanded: bool) -> Result<()> {
        unsafe {
            let interop: ICompositionDrawingSurfaceInterop = self.surface.cast()?;
            let mut off = POINT::default();
            let ctx: ID2D1DeviceContext = interop.BeginDraw(None, &mut off)?;
            ctx.SetTransform(&Matrix3x2::translation(off.x as f32, off.y as f32));
            ctx.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));

            let s = self.scale;
            let fg = ctx.CreateSolidColorBrush(&color(0.95, 0.95, 0.96), None)?;
            let muted = ctx.CreateSolidColorBrush(&color(0.55, 0.57, 0.61), None)?;
            let blue = ctx.CreateSolidColorBrush(&color(0.43, 0.55, 1.0), None)?;
            let green = ctx.CreateSolidColorBrush(&color(0.27, 0.83, 0.56), None)?;
            let track = ctx.CreateSolidColorBrush(&color(0.12, 0.13, 0.16), None)?;

            let fmt = |size: f32, weight: DWRITE_FONT_WEIGHT| -> Result<IDWriteTextFormat> {
                let f = self.dwrite.CreateTextFormat(
                    w!("Segoe UI Variable Text"),
                    None,
                    weight,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    size * s,
                    w!("en-us"),
                )?;
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                Ok(f)
            };
            let text = |t: &str, f: &IDWriteTextFormat, r: D2D_RECT_F, b: &ID2D1SolidColorBrush| {
                let wide: Vec<u16> = t.encode_utf16().collect();
                ctx.DrawText(&wide, f, &r, b, D2D1_DRAW_TEXT_OPTIONS_NONE, DWRITE_MEASURING_MODE_NATURAL);
            };

            let (w, _, _) = self.dims(expanded);
            let left = (self.max_w() - w) / 2.0;
            if !expanded {
                ctx.FillEllipse(
                    &D2D1_ELLIPSE {
                        point: Vector2 { X: left + 18.0 * s, Y: 17.0 * s },
                        radiusX: 4.0 * s,
                        radiusY: 4.0 * s,
                    },
                    &blue,
                );
                let f = fmt(13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
                f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
                text("09:41", &f, rect(left, 0.0, left + w - 14.0 * s, 34.0 * s), &fg);
            } else {
                let body = fmt(12.5, DWRITE_FONT_WEIGHT_NORMAL)?;
                let head = fmt(10.5, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
                let x0 = left + 14.0 * s;
                let x1 = left + 250.0 * s;
                text("Rabu, 22 Okt", &body, rect(x0, 0.0, x0 + 200.0 * s, 34.0 * s), &muted);
                text("HARI INI · 5", &head, rect(x0, 40.0 * s, x0 + 220.0 * s, 56.0 * s), &muted);
                let tasks = ["Standup tim", "Review PR #214 auth refactor", "Meeting sprint 14:00", "Balas email klien invoice", "Update dokumentasi API"];
                for (i, t) in tasks.iter().enumerate() {
                    let y = 60.0 * s + i as f32 * 22.0 * s;
                    text(t, &body, rect(x0, y, x0 + 220.0 * s, y + 20.0 * s), &fg);
                }
                text("AGENT · CLAUDE CODE", &head, rect(x1, 40.0 * s, x1 + 220.0 * s, 56.0 * s), &muted);
                let steps = ["Read auth.service.ts", "Edit refreshSession()", "Bash · npm test"];
                for (i, t) in steps.iter().enumerate() {
                    let y = 60.0 * s + i as f32 * 22.0 * s;
                    text(t, &body, rect(x1, y, x1 + 220.0 * s, y + 20.0 * s), &fg);
                }
                for (i, (label, pct, brush)) in [("Sesi 5 jam · 42%", 0.42, &blue), ("Mingguan · 18%", 0.18, &green)].iter().enumerate() {
                    let y = 132.0 * s + i as f32 * 34.0 * s;
                    text(label, &body, rect(x1, y, x1 + 220.0 * s, y + 18.0 * s), &muted);
                    let bar = rect(x1, y + 21.0 * s, x1 + 200.0 * s, y + 26.0 * s);
                    ctx.FillRectangle(&bar, &track);
                    ctx.FillRectangle(&rect(bar.left, bar.top, bar.left + 200.0 * s * pct, bar.bottom), *brush);
                }
            }
            interop.EndDraw()
        }
    }

    /// The window is only as big as its shape, so the area outside the pill stays clickable without cursor polling.
    fn set_window(&self, full: bool) -> Result<()> {
        let max_w = self.max_w();
        let (x, w, h, root_x) = if full {
            (self.max_rect.left, max_w as i32, self.max_rect.bottom - self.max_rect.top, 0.0)
        } else {
            let (cw, ch, _) = self.dims(false);
            let x = self.max_rect.left + ((max_w - cw) / 2.0).floor() as i32;
            (x, cw.ceil() as i32, ch.ceil() as i32, -((x - self.max_rect.left) as f32))
        };
        self.root.SetOffset(Vector3 { X: root_x, Y: 0.0, Z: 0.0 })?;
        unsafe { SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, self.max_rect.top, w, h, SWP_NOACTIVATE) }
    }

    fn toggle(&mut self) -> Result<()> {
        self.expanded = !self.expanded;
        unsafe {
            let _ = KillTimer(Some(self.hwnd), ID_SHRINK);
        }
        if self.expanded {
            self.set_window(true)?;
        }
        self.apply_shape(self.expanded, true)?;
        self.draw_text(self.expanded)?;
        self.fade_in_text()?;
        if !self.expanded {
            unsafe { SetTimer(Some(self.hwnd), ID_SHRINK, (ANIM_MS + 40) as u32, None) };
        }
        Ok(())
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_LBUTTONUP => {
                with_app(|a| a.toggle());
                LRESULT(0)
            }
            WM_RBUTTONUP | WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_TIMER => {
                match wp.0 {
                    ID_SHRINK => {
                        let _ = KillTimer(Some(hwnd), ID_SHRINK);
                        with_app(|a| a.set_window(false));
                    }
                    ID_CYCLE => with_app(|a| a.toggle()),
                    _ => {}
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn cycle_arg() -> Option<u32> {
    let args: Vec<String> = std::env::args().collect();
    let i = args.iter().position(|a| a == "--cycle")?;
    args.get(i + 1)?.parse().ok()
}

fn main() -> Result<()> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)?;
        let _queue: DispatcherQueueController = CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_STA,
        })?;

        let instance = GetModuleHandleW(None)?;
        let class = w!("NotchPillComposition");
        let wc = WNDCLASSW {
            hCursor: LoadCursorW(None, IDC_HAND)?,
            hInstance: instance.into(),
            lpszClassName: class,
            lpfnWndProc: Some(wndproc),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("Notch Pill (Composition)"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance.into()),
            None,
        )?;

        let scale = GetDpiForWindow(hwnd) as f32 / 96.0;
        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let max_w = (EXPANDED.0 * scale).ceil() as i32;
        let max_h = (EXPANDED.1 * scale).ceil() as i32;
        let left = (screen_w - max_w) / 2;
        let max_rect = RECT { left, top: 0, right: left + max_w, bottom: max_h };

        let compositor = Compositor::new()?;
        let target = compositor.cast::<ICompositorDesktopInterop>()?.CreateDesktopWindowTarget(hwnd, true)?;
        let root = compositor.CreateContainerVisual()?;
        target.SetRoot(&root)?;

        let geom = compositor.CreateRoundedRectangleGeometry()?;
        let shape = compositor.CreateSpriteShapeWithGeometry(&geom)?;
        shape.SetFillBrush(&compositor.CreateColorBrushWithColor(Color { A: 255, R: 0, G: 0, B: 0 })?)?;
        let shape_visual = compositor.CreateShapeVisual()?;
        shape_visual.SetSize(Vector2 { X: max_w as f32, Y: max_h as f32 })?;
        shape_visual.Shapes()?.Append(&shape)?;
        root.Children()?.InsertAtTop(&shape_visual)?;

        // Text is drawn by Direct2D/DirectWrite onto the Composition surface.
        let mut d3d: Option<ID3D11Device> = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut d3d),
            None,
            None,
        )?;
        let dxgi: IDXGIDevice = d3d.unwrap().cast()?;
        let d2d_factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let d2d_device = d2d_factory.CreateDevice(&dxgi)?;
        let graphics = compositor.cast::<ICompositorInterop>()?.CreateGraphicsDevice(&d2d_device)?;
        let surface = graphics.CreateDrawingSurface(
            Size { Width: max_w as f32, Height: max_h as f32 },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        let text_visual = compositor.CreateSpriteVisual()?;
        text_visual.SetSize(Vector2 { X: max_w as f32, Y: max_h as f32 })?;
        text_visual.SetBrush(&compositor.CreateSurfaceBrushWithSurface(&surface)?)?;
        root.Children()?.InsertAtTop(&text_visual)?;

        let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

        APP.with(|a| {
            *a.borrow_mut() = Some(App {
                hwnd,
                scale,
                expanded: false,
                max_rect,
                compositor,
                root,
                geom,
                text_visual,
                surface,
                dwrite,
                _target: target,
                _graphics: graphics,
            })
        });
        with_app(|a| {
            a.apply_shape(false, false)?;
            a.draw_text(false)?;
            a.set_window(false)
        });
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

        if let Some(ms) = cycle_arg() {
            SetTimer(Some(hwnd), ID_CYCLE, ms, None);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        Ok(())
    }
}
