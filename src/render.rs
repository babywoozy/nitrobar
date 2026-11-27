//! Direct2D / DirectWrite drawing. All coordinates here are DIPs; the render
//! target's DPI is set to the monitor DPI so Direct2D does the scaling.
//!
//! With a translucent background (clear / blur / acrylic) pixels are cleared to
//! transparent and DWM composes what is behind the bar underneath; text then uses
//! grayscale antialiasing (ClearType needs an opaque background) and, over an
//! unknown wallpaper, a soft shadow.

use std::collections::HashMap;
use std::mem::ManuallyDrop;

use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_BEZIER_SEGMENT, D2D1_COLOR_F,
    D2D1_FIGURE_BEGIN_FILLED, D2D1_FIGURE_END_CLOSED, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_LAYER_PARAMETERS, D2D1_PRESENT_OPTIONS_NONE,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory,
    ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1Layer, ID2D1PathGeometry,
    ID2D1RenderTarget, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT,
    DWRITE_FONT_WEIGHT_LIGHT, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_FONT_WEIGHT_SEMI_LIGHT, DWRITE_LINE_METRICS, DWRITE_MEASURING_MODE_NATURAL,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_PARAGRAPH_ALIGNMENT_NEAR, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
    DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
    IDWriteTypography,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{BOOL, Interface, PCWSTR, Result, w};
use windows_numerics::{Matrix3x2, Vector2};

use windows::Win32::Graphics::Imaging::IWICBitmap;

use crate::clock::ClockText;
use crate::config::Background;
use crate::icons::Icon;
use crate::island::{BAR_GAP, BAR_WIDTH, Reveal};
use crate::layout::{self, Metrics};
use crate::media::BARS;
use crate::shape;
use crate::status::StatusView;
use crate::theme::Palette;

const TRANSPARENT: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.0,
};
const CLOCK_FONT_SIZE: f32 = 12.0;
const TIME_FONT_SIZE: f32 = 15.0;
/// Cache key bit of an icon's smaller copy.
const SMALL_ICON: u64 = 1 << 62;
/// Cache key bit of an icon's larger copy.
const LARGE_ICON: u64 = 1 << 63;
/// Height of Segoe UI's digits relative to the font size.
const DIGIT_HEIGHT: f32 = 0.70;
/// Island shadow: this many stacked, slightly growing translucent copies of the
/// panel, `SHADOW_STEP` DIPs apart, shifted down by `SHADOW_DROP`.
const SHADOW_LAYERS: u32 = 8;
const SHADOW_STEP: f32 = 1.5;
const SHADOW_DROP: f32 = 3.0;
const SHADOW_ALPHA: f32 = 0.03;
/// Offsets (DIPs) of the faint copies that form the text halo.
const SHADOW_RING: [(f32, f32); 8] = [
    (-1.0, 0.0),
    (1.0, 0.0),
    (0.0, -1.0),
    (0.0, 1.0),
    (-0.7, -0.7),
    (0.7, -0.7),
    (-0.7, 0.7),
    (0.7, 0.7),
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    /// Pinned but not running: no indicator.
    Pinned,
    Running,
    Active,
    Attention,
    /// Running, but all its windows are minimized: a faint dot.
    Minimized,
}

/// Pointer feedback for any clickable element.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Pointer {
    pub hover: bool,
    pub pressed: bool,
}

pub struct ButtonView<'a> {
    pub rect: D2D_RECT_F,
    pub icon: Option<&'a Icon>,
    pub state: ButtonState,
    pub pointer: Pointer,
    /// Icon magnification (1 = normal) and how far the icon is lifted, DIPs
    /// (island: the dock-like zoom under the pointer).
    pub scale: f32,
    pub lift: f32,
    /// Island: how it rolls in / out while the island opens or closes.
    pub reveal: Reveal,
    /// The icon is drawn elsewhere (it is in flight): only plate and indicator.
    pub hide_icon: bool,
}

pub struct Scene<'a> {
    pub metrics: Metrics,
    pub palette: Palette,
    /// What DWM shows under transparent pixels.
    pub backdrop: Background,
    pub start: (D2D_RECT_F, Pointer),
    pub buttons: Vec<ButtonView<'a>>,
    pub status_rect: D2D_RECT_F,
    pub status_pointer: Pointer,
    pub status: StatusView,
    pub clock_rect: D2D_RECT_F,
    pub clock_pointer: Pointer,
    pub clock: ClockText,
    /// Island: line between the app buttons and the status area.
    pub divider: Option<D2D_RECT_F>,
    pub status_reveal: Reveal,
    pub clock_reveal: Reveal,
    /// Island: an app icon on its way between the resting pill and its button.
    pub flight: Option<(Icon, D2D_RECT_F)>,
    /// Island: zoom and roll-in of the Start button.
    pub start_scale: f32,
    pub start_reveal: Reveal,
    /// DIP offset of the window inside bar space (island: the collapsed window is
    /// only the pill area).
    pub origin: (f32, f32),
    /// Island mode: the morphing panel drawn behind (and clipping) the content.
    pub island: Option<IslandView>,
}

/// The island panel at one point of the expand / collapse animation.
pub struct IslandView {
    pub panel: D2D_RECT_F,
    pub radius: f32,
    pub fill: D2D1_COLOR_F,
    pub border: D2D1_COLOR_F,
    /// Whether the strip content is drawn (the island is open or moving), and
    /// whether the panel's outline clips it (while moving).
    pub content: bool,
    pub clip: bool,
    /// Continuous (superellipse) corners instead of circular ones.
    pub squircle: bool,
    /// Soft shadow strength under the panel, 0..1.
    pub shadow: f32,
    pub rest: Option<RestView>,
    /// App icon inside the pill while an app goes in / comes out.
    pub badge: Option<Badge>,
}

/// What the pill shows while it is not expanded: the mascot and the time, or the
/// music that is playing (crossfaded by `music_mix`).
pub struct RestView {
    pub opacity: f32,
    /// 0 = mascot and clock, 1 = music.
    pub music_mix: f32,
    pub mascot: Option<MascotView>,
    /// Small icons of the active app (two while one morphs into the next) with
    /// how each looks, centered in `app_rect`.
    pub apps: Vec<(Icon, Reveal)>,
    pub app_rect: D2D_RECT_F,
    pub clock: Option<(Vec<u16>, D2D_RECT_F)>,
    pub music: Option<MusicView>,
    pub text: D2D1_COLOR_F,
    pub secondary: D2D1_COLOR_F,
    pub accent: D2D1_COLOR_F,
}

pub struct MascotView {
    /// Less than 1 while it gives way to an app icon.
    pub opacity: f32,
    /// Square box (bar space) the flame fills.
    pub rect: D2D_RECT_F,
    pub asleep: bool,
    /// Size factor (the wake-up hop).
    pub scale: f32,
    /// Phase of the floating "z" while asleep.
    pub z: Option<f32>,
}

pub struct MusicView {
    /// Round cover art; without one a plain accent disc.
    pub art: Option<Icon>,
    pub art_rect: D2D_RECT_F,
    pub title: Vec<u16>,
    pub title_rect: D2D_RECT_F,
    pub artist: Vec<u16>,
    pub artist_rect: D2D_RECT_F,
    /// Equalizer bar heights 0..1 and their box.
    pub bars: [f32; BARS],
    pub bars_rect: D2D_RECT_F,
}

pub struct Badge {
    pub icon: Icon,
    /// Icon box in bar space (DIPs); drawn at the icon's pixel size, centered.
    pub rect: D2D_RECT_F,
    pub opacity: f32,
}

impl Scene<'_> {
    fn translucent(&self) -> bool {
        self.backdrop != Background::Solid || self.island.is_some()
    }

    /// Corner radius of a hover plate: nested inside the round island, else small.
    fn plate_radius(&self, rect: D2D_RECT_F) -> f32 {
        if self.island.is_some() {
            (rect.bottom - rect.top) / 2.0
        } else {
            layout::CORNER_RADIUS
        }
    }

    /// Shadow color for text and glyphs when the bar shows the wallpaper (clear or
    /// light blur): dark under light text, light under dark text.
    fn text_shadow(&self) -> Option<D2D1_COLOR_F> {
        // The island is a solid dark panel: nothing to protect the text from.
        if self.island.is_some() || !matches!(self.backdrop, Background::Clear | Background::Blur) {
            return None;
        }
        let t = self.palette.text;
        let light_text = 0.299 * t.r + 0.587 * t.g + 0.114 * t.b > 0.5;
        let v = if light_text { 0.0 } else { 1.0 };
        Some(D2D1_COLOR_F {
            r: v,
            g: v,
            b: v,
            a: 1.0,
        })
    }
}

/// Device-dependent resources, recreated when the device is lost.
struct Target {
    rt: ID2D1RenderTarget,
    /// The window to resize (`None` for an offscreen target).
    window: Option<ID2D1HwndRenderTarget>,
    size: D2D_SIZE_U,
    brush: ID2D1SolidColorBrush,
    /// Icon bitmaps by `Icon::id`; pruned to the icons drawn in the last frame.
    bitmaps: HashMap<u64, ID2D1Bitmap>,
    /// Reused for the island fade (an anonymous layer is re-allocated every frame).
    layer: Option<ID2D1Layer>,
}

pub struct Renderer {
    d2d: ID2D1Factory,
    clock_format: IDWriteTextFormat,
    icon_format: IDWriteTextFormat,
    percent_format: IDWriteTextFormat,
    /// Pill: song title (semibold) and artist (smaller), the time (semilight).
    title_format: IDWriteTextFormat,
    sub_format: IDWriteTextFormat,
    time_format: IDWriteTextFormat,
    /// Island strip: time over date.
    strip_time_format: IDWriteTextFormat,
    strip_date_format: IDWriteTextFormat,
    dwrite: IDWriteFactory,
    /// Figures of equal width, so the time does not jiggle.
    tabular: IDWriteTypography,
    /// The mascot: outer flame and inner core, in a 24 x 24 box.
    flame: ID2D1PathGeometry,
    core: ID2D1PathGeometry,
    target: Option<Target>,
}

/// "Segoe Fluent Icons" (Windows 11), else "Segoe MDL2 Assets" (same code points).
fn icon_font_family(dwrite: &IDWriteFactory) -> PCWSTR {
    let fluent = w!("Segoe Fluent Icons");
    let found = unsafe {
        let mut collection = None;
        if dwrite
            .GetSystemFontCollection(&raw mut collection, false)
            .is_err()
        {
            return fluent;
        }
        let mut index = 0u32;
        let mut exists = BOOL::default();
        collection.is_some_and(|c| {
            c.FindFamilyName(fluent, &raw mut index, &raw mut exists)
                .is_ok()
                && exists.as_bool()
        })
    };
    if found {
        fluent
    } else {
        w!("Segoe MDL2 Assets")
    }
}

/// A closed filled shape through `points`.
fn polygon(d2d: &ID2D1Factory, points: &[(f32, f32)]) -> Result<ID2D1PathGeometry> {
    unsafe {
        let geometry = d2d.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        let mut vertices = points.iter().map(|&(x, y)| Vector2 { X: x, Y: y });
        if let Some(first) = vertices.next() {
            sink.BeginFigure(first, D2D1_FIGURE_BEGIN_FILLED);
            let rest: Vec<Vector2> = vertices.collect();
            sink.AddLines(&rest);
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        }
        sink.Close()?;
        Ok(geometry)
    }
}

/// A closed filled shape from a start point and cubic Bezier segments.
fn path(
    d2d: &ID2D1Factory,
    start: (f32, f32),
    curves: &[[(f32, f32); 3]],
) -> Result<ID2D1PathGeometry> {
    let v = |(x, y): (f32, f32)| Vector2 { X: x, Y: y };
    unsafe {
        let geometry = d2d.CreatePathGeometry()?;
        let sink = geometry.Open()?;
        sink.BeginFigure(v(start), D2D1_FIGURE_BEGIN_FILLED);
        for [a, b, c] in curves {
            sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                point1: v(*a),
                point2: v(*b),
                point3: v(*c),
            });
        }
        sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        sink.Close()?;
        Ok(geometry)
    }
}

fn weighted_format(
    dwrite: &IDWriteFactory,
    family: PCWSTR,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
    align: DWRITE_TEXT_ALIGNMENT,
) -> Result<IDWriteTextFormat> {
    unsafe {
        let format = dwrite.CreateTextFormat(
            family,
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!(""),
        )?;
        format.SetTextAlignment(align)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        Ok(format)
    }
}

fn text_format(
    dwrite: &IDWriteFactory,
    family: PCWSTR,
    size: f32,
    align: DWRITE_TEXT_ALIGNMENT,
) -> Result<IDWriteTextFormat> {
    unsafe {
        let format = dwrite.CreateTextFormat(
            family,
            None,
            DWRITE_FONT_WEIGHT_NORMAL,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            w!(""),
        )?;
        format.SetTextAlignment(align)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        Ok(format)
    }
}

impl Renderer {
    pub fn new() -> Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let ui_font = w!("Segoe UI Variable Text");
            // Two-line texts are cut with an ellipsis.
            let trimmed = |size: f32, weight: DWRITE_FONT_WEIGHT| -> Result<IDWriteTextFormat> {
                let format = weighted_format(
                    &dwrite,
                    ui_font,
                    size,
                    weight,
                    DWRITE_TEXT_ALIGNMENT_LEADING,
                )?;
                let sign = dwrite.CreateEllipsisTrimmingSign(&format)?;
                let trimming = DWRITE_TRIMMING {
                    granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                    delimiter: 0,
                    delimiterCount: 0,
                };
                format.SetTrimming(&raw const trimming, &sign)?;
                Ok(format)
            };
            let title_format = trimmed(13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD)?;
            let sub_format = trimmed(11.5, DWRITE_FONT_WEIGHT_NORMAL)?;
            let time_format = weighted_format(
                &dwrite,
                ui_font,
                TIME_FONT_SIZE,
                DWRITE_FONT_WEIGHT_LIGHT,
                DWRITE_TEXT_ALIGNMENT_TRAILING,
            )?;
            let strip_time_format = weighted_format(
                &dwrite,
                ui_font,
                13.0,
                DWRITE_FONT_WEIGHT_SEMI_LIGHT,
                DWRITE_TEXT_ALIGNMENT_TRAILING,
            )?;
            let strip_date_format =
                text_format(&dwrite, ui_font, 11.0, DWRITE_TEXT_ALIGNMENT_TRAILING)?;
            let tabular = dwrite.CreateTypography()?;
            tabular.AddFontFeature(DWRITE_FONT_FEATURE {
                nameTag: DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES,
                parameter: 1,
            })?;
            let flame = path(
                &d2d,
                (12.0, 1.0),
                &[
                    [(13.0, 6.0), (21.0, 9.0), (21.0, 15.5)],
                    [(21.0, 20.5), (17.0, 23.0), (12.0, 23.0)],
                    [(7.0, 23.0), (3.0, 20.5), (3.0, 15.5)],
                    [(3.0, 11.5), (6.0, 9.5), (7.2, 6.6)],
                    [(8.0, 8.8), (8.8, 9.8), (10.0, 10.2)],
                    [(9.4, 6.6), (10.4, 3.2), (12.0, 1.0)],
                ],
            )?;
            let core = path(
                &d2d,
                (12.6, 9.8),
                &[
                    [(14.6, 12.6), (18.0, 14.2), (18.0, 17.4)],
                    [(18.0, 20.4), (15.4, 21.8), (12.0, 21.8)],
                    [(8.6, 21.8), (6.0, 20.4), (6.0, 17.4)],
                    [(6.0, 14.4), (11.0, 13.4), (12.6, 9.8)],
                ],
            )?;
            Ok(Self {
                title_format,
                sub_format,
                time_format,
                strip_time_format,
                strip_date_format,
                tabular,
                flame,
                core,
                clock_format: text_format(
                    &dwrite,
                    ui_font,
                    CLOCK_FONT_SIZE,
                    DWRITE_TEXT_ALIGNMENT_TRAILING,
                )?,
                icon_format: text_format(
                    &dwrite,
                    icon_font_family(&dwrite),
                    layout::STATUS_GLYPH_SIZE,
                    DWRITE_TEXT_ALIGNMENT_CENTER,
                )?,
                percent_format: text_format(
                    &dwrite,
                    ui_font,
                    CLOCK_FONT_SIZE,
                    DWRITE_TEXT_ALIGNMENT_LEADING,
                )?,
                d2d,
                dwrite,
                target: None,
            })
        }
    }

    fn client_size(hwnd: HWND) -> Result<D2D_SIZE_U> {
        let mut rc = RECT::default();
        unsafe { GetClientRect(hwnd, &raw mut rc)? };
        Ok(D2D_SIZE_U {
            width: (rc.right - rc.left).max(0) as u32,
            height: (rc.bottom - rc.top).max(0) as u32,
        })
    }

    fn create_target(&self, hwnd: HWND, size: D2D_SIZE_U, dpi: f32) -> Result<Target> {
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            // A GPU target pulls in a D3D11 device + driver (~30 MB private memory).
            // The bar redraws a few small regions per minute, so the CPU rasterizer
            // is both lighter and fast enough.
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: dpi,
            dpiY: dpi,
            ..Default::default()
        };
        let hwnd_props = D2D1_HWND_RENDER_TARGET_PROPERTIES {
            hwnd,
            pixelSize: size,
            presentOptions: D2D1_PRESENT_OPTIONS_NONE,
        };
        unsafe {
            let rt = self
                .d2d
                .CreateHwndRenderTarget(&raw const props, &raw const hwnd_props)?;
            let brush = rt.CreateSolidColorBrush(&TRANSPARENT, None)?;
            Ok(Target {
                layer: rt.CreateLayer(None).ok(),
                window: Some(rt.clone()),
                rt: rt.into(),
                size,
                brush,
                bitmaps: HashMap::new(),
            })
        }
    }

    /// Drops the render target; the next paint recreates it.
    pub fn reset(&mut self) {
        self.target = None;
    }

    /// Draws the whole bar. Call between `BeginPaint` and `EndPaint`.
    pub fn draw(&mut self, hwnd: HWND, dpi: u32, scene: &Scene) -> Result<()> {
        let size = Self::client_size(hwnd)?;
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let dpi = dpi as f32;

        let mut target = match self.target.take() {
            Some(mut t) => {
                if t.size != size {
                    if let Some(window) = &t.window {
                        unsafe { window.Resize(&raw const size)? };
                    }
                    t.size = size;
                }
                t
            }
            None => self.create_target(hwnd, size, dpi)?,
        };

        self.render(&mut target, scene, dpi);

        match unsafe { target.rt.EndDraw(None, None) } {
            // Device lost: drop the target and paint again right away with a new one
            // (otherwise the window keeps whatever it showed, e.g. black).
            Err(e) if e.code() == D2DERR_RECREATE_TARGET => {
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                Ok(())
            }
            Err(e) => Err(e),
            Ok(()) => {
                self.target = Some(target);
                Ok(())
            }
        }
    }

    /// `BeginDraw` and everything in the scene; the caller ends the draw.
    fn render(&self, target: &mut Target, scene: &Scene, dpi: f32) {
        let palette = &scene.palette;
        unsafe {
            target.rt.SetDpi(dpi, dpi);
            target.rt.SetTextAntialiasMode(if scene.translucent() {
                D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE
            } else {
                D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE
            });
            target.rt.BeginDraw();
            target.rt.Clear(Some(if scene.translucent() {
                &TRANSPARENT
            } else {
                &palette.background
            }));
            // Scene coordinates are bar space; the window may be only part of it.
            let origin = Matrix3x2::translation(-scene.origin.0, -scene.origin.1);
            target.rt.SetTransform(&raw const origin);
            // Acrylic is tinted by DWM itself (accent gradient color).
            if scene.island.is_none()
                && matches!(scene.backdrop, Background::Clear | Background::Blur)
                && palette.background.a > 0.0
            {
                let full = D2D_RECT_F {
                    left: 0.0,
                    top: 0.0,
                    right: 100_000.0,
                    bottom: 100_000.0,
                };
                target.brush.SetColor(&raw const palette.background);
                target.rt.FillRectangle(&raw const full, &target.brush);
            }
        }

        let mut used = Vec::new();
        let content = match &scene.island {
            Some(island) => self.draw_island(target, island, dpi, scene.origin, &mut used),
            None => true,
        };
        if content {
            // While the island moves, its outline is a mask: the content stays put
            // and the edge uncovers or swallows it.
            let mask = scene
                .island
                .as_ref()
                .filter(|island| island.clip)
                .and_then(|island| {
                    let points = shape::pill(&island.panel);
                    polygon(&self.d2d, &points).ok().map(|g| (island.panel, g))
                });
            if let Some((bounds, geometry)) = &mask {
                let mut params = D2D1_LAYER_PARAMETERS {
                    contentBounds: *bounds,
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: Matrix3x2::identity(),
                    opacity: 1.0,
                    ..Default::default()
                };
                params.geometricMask = ManuallyDrop::new(geometry.cast().ok());
                unsafe {
                    target
                        .rt
                        .PushLayer(&raw const params, target.layer.as_ref())
                };
                drop(ManuallyDrop::into_inner(params.geometricMask));
            }
            self.draw_content(target, scene, dpi, &mut used);
            if mask.is_some() {
                unsafe { target.rt.PopLayer() };
            }
        }
        if let Some((icon, rect)) = &scene.flight {
            used.push(icon.id | LARGE_ICON);
            if let Some(bitmap) = Self::cached_bitmap(target, icon.id | LARGE_ICON, &icon.large) {
                unsafe {
                    target.rt.DrawBitmap(
                        &bitmap,
                        Some(rect),
                        1.0,
                        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                        None,
                    );
                }
            }
        }
        target.bitmaps.retain(|id, _| used.contains(id));
    }

    /// The island panel (pill, or the bar while morphing). Returns the opacity
    /// of the bar content to draw on top (0 while collapsed).
    fn draw_island(
        &self,
        target: &mut Target,
        island: &IslandView,
        dpi: f32,
        origin: (f32, f32),
        used: &mut Vec<u64>,
    ) -> bool {
        let rounded = D2D1_ROUNDED_RECT {
            rect: island.panel,
            radiusX: island.radius,
            radiusY: island.radius,
        };
        let outline = if island.squircle {
            shape::pill(&island.panel)
        } else {
            Vec::new()
        };
        let squircle = (!outline.is_empty())
            .then(|| polygon(&self.d2d, &outline).ok())
            .flatten();
        unsafe {
            if island.shadow > 0.0 {
                // Strokes of growing width, faintest summed furthest out: a soft
                // falloff around the shape (the inner halves end up under the fill).
                let color = D2D1_COLOR_F {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: SHADOW_ALPHA * island.shadow,
                };
                target.brush.SetColor(&raw const color);
                let drop = Matrix3x2::translation(-origin.0, SHADOW_DROP - origin.1);
                target.rt.SetTransform(&raw const drop);
                for i in 1..=SHADOW_LAYERS {
                    let width = 2.0 * i as f32 * SHADOW_STEP;
                    match &squircle {
                        Some(geometry) => {
                            target.rt.DrawGeometry(geometry, &target.brush, width, None)
                        }
                        None => target.rt.DrawRoundedRectangle(
                            &raw const rounded,
                            &target.brush,
                            width,
                            None,
                        ),
                    }
                }
                let reset = Matrix3x2::translation(-origin.0, -origin.1);
                target.rt.SetTransform(&raw const reset);
            }
            if island.fill.a > 0.0 {
                target.brush.SetColor(&raw const island.fill);
                match &squircle {
                    Some(geometry) => target.rt.FillGeometry(geometry, &target.brush, None),
                    None => target
                        .rt
                        .FillRoundedRectangle(&raw const rounded, &target.brush),
                }
            }
            if island.border.a > 0.0 {
                // Centered half a DIP inside the edge, so it covers whole pixels.
                let inner = D2D_RECT_F {
                    left: island.panel.left + 0.5,
                    top: island.panel.top + 0.5,
                    right: island.panel.right - 0.5,
                    bottom: island.panel.bottom - 0.5,
                };
                target.brush.SetColor(&raw const island.border);
                if island.squircle {
                    if let Ok(geometry) = polygon(&self.d2d, &shape::pill(&inner)) {
                        target.rt.DrawGeometry(&geometry, &target.brush, 1.0, None);
                    }
                } else {
                    let line = D2D1_ROUNDED_RECT {
                        rect: inner,
                        radiusX: (island.radius - 0.5).max(0.0),
                        radiusY: (island.radius - 0.5).max(0.0),
                    };
                    target
                        .rt
                        .DrawRoundedRectangle(&raw const line, &target.brush, 1.0, None);
                }
            }
        }
        if let Some(rest) = &island.rest
            && rest.opacity > 0.0
        {
            let idle = rest.opacity * (1.0 - rest.music_mix);
            let music = rest.opacity * rest.music_mix;
            if idle > 0.01 {
                if let Some(mascot) = &rest.mascot {
                    self.draw_mascot(target, rest, mascot, origin, idle * mascot.opacity);
                }
                for (icon, look) in &rest.apps {
                    used.push(icon.id | SMALL_ICON);
                    let key = icon.id | SMALL_ICON;
                    if let Some(bitmap) = Self::cached_bitmap(target, key, &icon.small) {
                        // Settled: 1:1 on whole pixels; morphing: transformed.
                        Self::apply_reveal(target, origin, &rest.app_rect, *look);
                        let at =
                            layout::pixel_square(&rest.app_rect, icon.small_px as f32, 0.0, dpi);
                        unsafe {
                            target.rt.DrawBitmap(
                                &bitmap,
                                Some(&raw const at),
                                idle * look.alpha,
                                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                                None,
                            );
                        }
                        Self::apply_reveal(target, origin, &rest.app_rect, Reveal::SHOWN);
                    }
                }
                if let Some((text, rect)) = &rest.clock {
                    let mut color = rest.text;
                    color.a *= idle;
                    self.draw_time_centered(
                        target,
                        text,
                        &self.time_format,
                        TIME_FONT_SIZE,
                        *rect,
                        &color,
                    );
                }
            }
            if music > 0.01
                && let Some(view) = &rest.music
            {
                self.draw_music(target, rest, view, music, dpi, used);
            }
        }
        if let Some(badge) = &island.badge
            && badge.opacity > 0.0
        {
            used.push(badge.icon.id);
            if let Some(bitmap) = Self::icon_bitmap(target, &badge.icon) {
                let px = badge.icon.px as f32;
                let rect = layout::pixel_square(&badge.rect, px, 0.0, dpi);
                unsafe {
                    target.rt.DrawBitmap(
                        &bitmap,
                        Some(&raw const rect),
                        badge.opacity,
                        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                        None,
                    );
                }
            }
        }
        island.content
    }

    /// Like `draw_time`, with the digits (not the line box) centered vertically.
    fn draw_time_centered(
        &self,
        target: &Target,
        text: &[u16],
        format: &IDWriteTextFormat,
        size: f32,
        rect: D2D_RECT_F,
        color: &D2D1_COLOR_F,
    ) {
        unsafe {
            let Ok(layout) =
                self.dwrite
                    .CreateTextLayout(text, format, rect.right - rect.left, size * 3.0)
            else {
                return;
            };
            let all = DWRITE_TEXT_RANGE {
                startPosition: 0,
                length: text.len() as u32,
            };
            let _ = layout.SetTypography(&self.tabular, all);
            let _ = layout.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
            let mut line = [DWRITE_LINE_METRICS::default()];
            let mut count = 0;
            if layout
                .GetLineMetrics(Some(&mut line), &raw mut count)
                .is_err()
            {
                return;
            }
            let middle = (rect.top + rect.bottom) / 2.0;
            let top = middle + size * DIGIT_HEIGHT / 2.0 - line[0].baseline;
            target.brush.SetColor(color);
            target.rt.DrawTextLayout(
                Vector2 {
                    X: rect.left,
                    Y: top,
                },
                &layout,
                &target.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }

    /// Width of one line of text, DIPs, rounded up.
    fn text_width(&self, text: &[u16], format: &IDWriteTextFormat) -> f32 {
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(text, format, 1000.0, 100.0) else {
                return 0.0;
            };
            let all = DWRITE_TEXT_RANGE {
                startPosition: 0,
                length: text.len() as u32,
            };
            let _ = layout.SetTypography(&self.tabular, all);
            let mut metrics = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&raw mut metrics).is_err() {
                return 0.0;
            }
            metrics.width.ceil()
        }
    }

    /// Widths the island strip is laid out with: the battery percentage and the
    /// wider of the clock's lines.
    pub fn strip_text(&self, percent: Option<&[u16]>, clock: &ClockText) -> (f32, f32) {
        let percent = percent.map_or(0.0, |p| self.text_width(p, &self.percent_format));
        let mut width = self.text_width(&clock.time, &self.strip_time_format);
        if !clock.date.is_empty() {
            width = width.max(self.text_width(&clock.date, &self.strip_date_format));
        }
        (percent, width)
    }

    /// Text laid out with tabular figures.
    fn draw_time(
        &self,
        target: &Target,
        text: &[u16],
        format: &IDWriteTextFormat,
        rect: D2D_RECT_F,
        color: &D2D1_COLOR_F,
    ) {
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(
                text,
                format,
                rect.right - rect.left,
                rect.bottom - rect.top,
            ) else {
                return;
            };
            let all = DWRITE_TEXT_RANGE {
                startPosition: 0,
                length: text.len() as u32,
            };
            let _ = layout.SetTypography(&self.tabular, all);
            target.brush.SetColor(color);
            target.rt.DrawTextLayout(
                Vector2 {
                    X: rect.left,
                    Y: rect.top,
                },
                &layout,
                &target.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
        }
    }

    fn fill_ellipse(target: &Target, x: f32, y: f32, rx: f32, ry: f32, color: &D2D1_COLOR_F) {
        let ellipse = D2D1_ELLIPSE {
            point: Vector2 { X: x, Y: y },
            radiusX: rx,
            radiusY: ry,
        };
        unsafe {
            target.brush.SetColor(color);
            target.rt.FillEllipse(&raw const ellipse, &target.brush);
        }
    }

    /// "Nitro", the flame with eyes; squashed with closed eyes and a "z" while asleep.
    fn draw_mascot(
        &self,
        target: &Target,
        rest: &RestView,
        mascot: &MascotView,
        origin: (f32, f32),
        opacity: f32,
    ) {
        let rect = mascot.rect;
        let unit = (rect.right - rect.left) / 24.0 * mascot.scale;
        let squash = if mascot.asleep { 0.84 } else { 1.0 };
        let dim = if mascot.asleep { 0.78 } else { 1.0 };
        let center = (rect.left + rect.right) / 2.0;
        // Anchored at the bottom: it hops and slumps from where it sits.
        let bottom = (rect.top + rect.bottom) / 2.0 + 11.0 * (rect.right - rect.left) / 24.0;
        let transform = Matrix3x2::translation(-12.0, -23.0)
            * Matrix3x2 {
                M11: unit,
                M12: 0.0,
                M21: 0.0,
                M22: unit * squash,
                M31: 0.0,
                M32: 0.0,
            }
            * Matrix3x2::translation(center - origin.0, bottom - origin.1);
        let mut body = rest.accent;
        body.a = opacity * dim;
        let mix = |c: f32| c + (1.0 - c) * 0.62;
        let inner = D2D1_COLOR_F {
            r: mix(rest.accent.r),
            g: mix(rest.accent.g),
            b: mix(rest.accent.b),
            a: opacity * dim,
        };
        let eye = D2D1_COLOR_F {
            r: 0.07,
            g: 0.07,
            b: 0.09,
            a: opacity,
        };
        unsafe {
            target.rt.SetTransform(&raw const transform);
            target.brush.SetColor(&raw const body);
            target.rt.FillGeometry(&self.flame, &target.brush, None);
            target.brush.SetColor(&raw const inner);
            target.rt.FillGeometry(&self.core, &target.brush, None);
        }
        for x in [9.4, 14.6] {
            if mascot.asleep {
                // Closed lids: a little smile-shaped line.
                unsafe {
                    target.brush.SetColor(&raw const eye);
                    for (from, to) in [(-1.5, 0.0), (0.0, 1.5)] {
                        let y = |dx: f32| if dx == 0.0 { 17.7 } else { 16.9 };
                        target.rt.DrawLine(
                            Vector2 {
                                X: x + from,
                                Y: y(from),
                            },
                            Vector2 {
                                X: x + to,
                                Y: y(to),
                            },
                            &target.brush,
                            1.15,
                            None,
                        );
                    }
                }
            } else {
                Self::fill_ellipse(target, x, 16.8, 1.45, 1.95, &eye);
                let glint = D2D1_COLOR_F {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: opacity,
                };
                Self::fill_ellipse(target, x + 0.45, 16.1, 0.6, 0.6, &glint);
            }
        }
        let reset = Matrix3x2::translation(-origin.0, -origin.1);
        unsafe { target.rt.SetTransform(&raw const reset) };
        if let Some(phase) = mascot.z {
            // A "z" drifting up and to the right, fading.
            let mut color = rest.secondary;
            color.a *= opacity * (1.0 - 0.6 * phase);
            let left = rect.right + 1.0 + 4.0 * phase;
            let top = rect.top + 2.0 - 6.0 * phase;
            let z = D2D_RECT_F {
                left,
                top,
                right: left + 12.0,
                bottom: top + 14.0,
            };
            self.draw_text(
                target,
                &[u16::from(b'z')],
                &self.sub_format,
                z,
                &color,
                None,
            );
        }
    }

    fn draw_music(
        &self,
        target: &mut Target,
        rest: &RestView,
        music: &MusicView,
        opacity: f32,
        dpi: f32,
        used: &mut Vec<u64>,
    ) {
        let faded = |mut c: D2D1_COLOR_F| {
            c.a *= opacity;
            c
        };
        let bitmap = music.art.as_ref().and_then(|art| {
            used.push(art.id);
            Self::icon_bitmap(target, art).map(|b| (b, art.px))
        });
        match bitmap {
            Some((bitmap, px)) => {
                let at = layout::pixel_square(&music.art_rect, px as f32, 0.0, dpi);
                unsafe {
                    target.rt.DrawBitmap(
                        &bitmap,
                        Some(&raw const at),
                        opacity,
                        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                        None,
                    );
                }
            }
            None => {
                let r = music.art_rect;
                let radius = (r.right - r.left) / 2.0;
                let color = faded(rest.accent);
                Self::fill_ellipse(
                    target,
                    r.left + radius,
                    r.top + radius,
                    radius,
                    radius,
                    &color,
                );
            }
        }
        self.draw_text(
            target,
            &music.title,
            &self.title_format,
            music.title_rect,
            &faded(rest.text),
            None,
        );
        self.draw_text(
            target,
            &music.artist,
            &self.sub_format,
            music.artist_rect,
            &faded(rest.secondary),
            None,
        );
        let r = music.bars_rect;
        let mid = (r.top + r.bottom) / 2.0;
        let accent = faded(rest.accent);
        for (i, level) in music.bars.iter().enumerate() {
            let left = r.left + i as f32 * (BAR_WIDTH + BAR_GAP);
            let h = BAR_WIDTH + level * (r.bottom - r.top - BAR_WIDTH);
            let bar = D2D_RECT_F {
                left,
                top: mid - h / 2.0,
                right: left + BAR_WIDTH,
                bottom: mid + h / 2.0,
            };
            Self::fill_rounded(target, bar, BAR_WIDTH / 2.0, &accent);
        }
    }

    /// Start, app buttons, status and clock.
    fn draw_content(&self, target: &mut Target, scene: &Scene, dpi: f32, used: &mut Vec<u64>) {
        let palette = &scene.palette;
        let (start, pointer) = scene.start;
        if scene.island.is_some() {
            // Monochrome like the rest of the island: it turns dark on light glass.
            if scene.start_reveal.alpha > 0.0 {
                Self::apply_reveal(target, scene.origin, &start, scene.start_reveal);
                let faded = palette.faded(scene.start_reveal.alpha);
                Self::draw_button_background(
                    target,
                    &faded,
                    start,
                    (start.bottom - start.top) / 2.0,
                    false,
                    pointer,
                );
                let mut size = scene.metrics.start_logo_size() * scene.start_scale;
                if pointer.pressed {
                    size *= 0.85;
                }
                Self::draw_logo(target, start, size, &faded.text, &faded.text);
            }
        } else if start.right > start.left {
            Self::draw_start(target, palette, &scene.metrics, start, pointer);
        }
        for button in &scene.buttons {
            if button.reveal.alpha > 0.0 {
                Self::apply_reveal(target, scene.origin, &button.rect, button.reveal);
                Self::draw_button(target, scene, dpi, button, used);
            }
        }
        if let Some(line) = &scene.divider {
            Self::apply_reveal(target, scene.origin, line, scene.status_reveal);
            let mut color = scene.palette.text;
            color.a = 0.18 * scene.status_reveal.alpha;
            Self::fill_rounded(target, *line, 0.5, &color);
        }
        if scene.status_reveal.alpha > 0.0 {
            Self::apply_reveal(
                target,
                scene.origin,
                &scene.status_rect,
                scene.status_reveal,
            );
            self.draw_status(target, scene);
        }
        if scene.clock_reveal.alpha > 0.0 {
            Self::apply_reveal(target, scene.origin, &scene.clock_rect, scene.clock_reveal);
            self.draw_clock(target, scene);
        }
        Self::apply_reveal(target, scene.origin, &D2D_RECT_F::default(), Reveal::SHOWN);
    }

    /// Transform for one piece of content: rolled around its center, on top of the
    /// bar-space origin.
    fn apply_reveal(target: &Target, origin: (f32, f32), rect: &D2D_RECT_F, reveal: Reveal) {
        let (x, y) = (
            (rect.left + rect.right) / 2.0,
            (rect.top + rect.bottom) / 2.0,
        );
        let (sin, cos) = reveal.angle.to_radians().sin_cos();
        let k = reveal.scale;
        let transform = Matrix3x2::translation(-x, -y)
            * Matrix3x2 {
                M11: k * cos,
                M12: k * sin,
                M21: -k * sin,
                M22: k * cos,
                M31: 0.0,
                M32: 0.0,
            }
            * Matrix3x2::translation(x + reveal.dx - origin.0, y - origin.1);
        unsafe { target.rt.SetTransform(&raw const transform) };
    }

    fn fill_rounded(target: &Target, rect: D2D_RECT_F, radius: f32, color: &D2D1_COLOR_F) {
        let rounded = D2D1_ROUNDED_RECT {
            rect,
            radiusX: radius,
            radiusY: radius,
        };
        unsafe {
            target.brush.SetColor(color);
            target
                .rt
                .FillRoundedRectangle(&raw const rounded, &target.brush);
        }
    }

    /// Hover / pressed / active background of a button.
    fn draw_button_background(
        target: &Target,
        palette: &Palette,
        rect: D2D_RECT_F,
        radius: f32,
        active: bool,
        pointer: Pointer,
    ) {
        let color = match (active, pointer.hover, pointer.pressed) {
            (_, _, true) => Some(&palette.pressed),
            (true, true, false) => Some(&palette.active_hover),
            (true, false, false) => Some(&palette.active),
            (false, true, false) => Some(&palette.hover),
            (false, false, false) => None,
        };
        if let Some(color) = color {
            Self::fill_plate(target, rect, radius, color);
        }
    }

    /// A hover / press plate: a squircle when it is fully round (the island's),
    /// else a rounded rectangle.
    fn fill_plate(target: &Target, rect: D2D_RECT_F, radius: f32, color: &D2D1_COLOR_F) {
        if radius < (rect.bottom - rect.top) / 2.0 {
            Self::fill_rounded(target, rect, radius, color);
            return;
        }
        unsafe {
            let Ok(factory) = target.rt.GetFactory() else {
                return;
            };
            if let Ok(geometry) = polygon(&factory, &shape::plate(&rect)) {
                target.brush.SetColor(color);
                target.rt.FillGeometry(&geometry, &target.brush, None);
            }
        }
    }

    /// The four-pane Windows logo, drawn as vectors so it is crisp at any DPI.
    fn draw_start(
        target: &Target,
        palette: &Palette,
        metrics: &Metrics,
        rect: D2D_RECT_F,
        pointer: Pointer,
    ) {
        Self::draw_button_background(target, palette, rect, layout::CORNER_RADIUS, false, pointer);
        let size = if pointer.pressed {
            metrics.start_logo_size() * 0.85
        } else {
            metrics.start_logo_size()
        };
        let top = D2D1_COLOR_F {
            r: 0.30,
            g: 0.78,
            b: 1.0,
            a: 1.0,
        };
        let bottom = D2D1_COLOR_F {
            r: 0.0,
            g: 0.56,
            b: 0.94,
            a: 1.0,
        };
        Self::draw_logo(target, rect, size, &top, &bottom);
    }

    /// Four panes, `size` square, centered in `rect`.
    fn draw_logo(
        target: &Target,
        rect: D2D_RECT_F,
        size: f32,
        top: &D2D1_COLOR_F,
        bottom: &D2D1_COLOR_F,
    ) {
        let logo = layout::centered_square(&rect, size, 0.0);
        // Whole-DIP panes and gap keep the edges crisp at 100 %.
        let gap = (size * 0.08).round().max(1.0);
        let pane = (size - gap) / 2.0;
        for row in 0..2 {
            for col in 0..2 {
                let left = logo.left + col as f32 * (pane + gap);
                let top_edge = logo.top + row as f32 * (pane + gap);
                let pane_rect = D2D_RECT_F {
                    left,
                    top: top_edge,
                    right: left + pane,
                    bottom: top_edge + pane,
                };
                let color = if row == 0 { top } else { bottom };
                Self::fill_rounded(target, pane_rect, size * 0.04, color);
            }
        }
    }

    fn icon_bitmap(target: &mut Target, icon: &Icon) -> Option<ID2D1Bitmap> {
        Self::cached_bitmap(target, icon.id, &icon.bitmap)
    }

    fn cached_bitmap(target: &mut Target, key: u64, source: &IWICBitmap) -> Option<ID2D1Bitmap> {
        if let Some(bitmap) = target.bitmaps.get(&key) {
            return Some(bitmap.clone());
        }
        let bitmap = unsafe { target.rt.CreateBitmapFromWicBitmap(source, None) }.ok()?;
        target.bitmaps.insert(key, bitmap.clone());
        Some(bitmap)
    }

    fn draw_button(
        target: &mut Target,
        scene: &Scene,
        dpi: f32,
        button: &ButtonView,
        used: &mut Vec<u64>,
    ) {
        let faded = scene.palette.faded(button.reveal.alpha);
        let palette = &faded;
        let metrics = &scene.metrics;
        let shadow = scene.text_shadow();
        let flat = scene.island.is_some();
        let rect = button.rect;
        // Inside the island's round panel the plates are round too (panel radius
        // minus the padding = half the button).
        let radius = if flat {
            (rect.bottom - rect.top) / 2.0
        } else {
            layout::CORNER_RADIUS
        };
        if button.state == ButtonState::Attention && !button.pointer.pressed {
            Self::fill_plate(target, rect, radius, &palette.attention_bg);
        } else if flat {
            // A faint round plate under the zoomed icon.
            if button.pointer.pressed {
                Self::fill_plate(target, rect, radius, &palette.pressed);
            } else if button.pointer.hover {
                Self::fill_plate(target, rect, radius, &palette.hover);
            }
        } else {
            Self::draw_button_background(
                target,
                palette,
                rect,
                radius,
                button.state == ButtonState::Active,
                button.pointer,
            );
        }

        let icon_size = if button.pointer.pressed {
            metrics.pressed_icon_size()
        } else {
            metrics.icon_size
        };
        // Nudge the icon away from the indicator side.
        let nudge = if metrics.at_top { 1.0 } else { -1.0 };
        // Icons are drawn at their own pixel size (never enlarged), on whole pixels:
        // 1:1 copies stay sharp at every DPI.
        let zoomed = button.scale > 1.01;
        let wanted_px = (icon_size * button.scale * dpi / 96.0).round();
        let icon_rect = if zoomed {
            // Magnified: scaled up from the icon bitmap (a bit soft), not on the pixel grid.
            layout::centered_square(&rect, wanted_px * 96.0 / dpi, nudge - button.lift)
        } else {
            let icon_px = button
                .icon
                .map_or(wanted_px, |icon| wanted_px.min(icon.px as f32));
            layout::pixel_square(&rect, icon_px, nudge - button.lift, dpi)
        };
        let bitmap = button.icon.and_then(|icon| {
            used.push(icon.id);
            if zoomed {
                // Reduced from the 2x copy: stays smooth while magnified.
                used.push(icon.id | LARGE_ICON);
                Self::cached_bitmap(target, icon.id | LARGE_ICON, &icon.large)
            } else {
                Self::icon_bitmap(target, icon)
            }
        });
        if !button.hide_icon {
            match bitmap {
                Some(bitmap) => unsafe {
                    target.rt.DrawBitmap(
                        &bitmap,
                        Some(&raw const icon_rect),
                        button.reveal.alpha,
                        D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                        None,
                    );
                },
                None => Self::fill_rounded(target, icon_rect, 4.0, &palette.placeholder),
            }
        }

        let faint = D2D1_COLOR_F {
            a: palette.indicator.a * 0.5,
            ..palette.indicator
        };
        let (width, color) = match button.state {
            ButtonState::Pinned => return,
            ButtonState::Running => (layout::INDICATOR_RUNNING_WIDTH, palette.indicator),
            ButtonState::Active => (layout::INDICATOR_ACTIVE_WIDTH, palette.accent),
            ButtonState::Attention => (layout::INDICATOR_ACTIVE_WIDTH, palette.attention),
            ButtonState::Minimized => (layout::INDICATOR_HEIGHT, faint),
        };
        let pill = layout::indicator(&rect, width, metrics.at_top);
        if let Some(mut halo) = shadow {
            // Keeps the pill visible on a wallpaper of the same brightness.
            halo.a = 0.35;
            let outer = D2D_RECT_F {
                left: pill.left - 1.0,
                top: pill.top - 1.0,
                right: pill.right + 1.0,
                bottom: pill.bottom + 1.0,
            };
            Self::fill_rounded(target, outer, layout::INDICATOR_HEIGHT / 2.0 + 1.0, &halo);
        }
        Self::fill_rounded(target, pill, layout::INDICATOR_HEIGHT / 2.0, &color);
    }

    /// Draws `text` in `color`; with `shadow`, first a soft halo + drop shadow so it
    /// stays readable over any wallpaper (clear / blur backgrounds).
    fn draw_text(
        &self,
        target: &Target,
        text: &[u16],
        format: &IDWriteTextFormat,
        rect: D2D_RECT_F,
        color: &D2D1_COLOR_F,
        shadow: Option<D2D1_COLOR_F>,
    ) {
        let draw = |dx: f32, dy: f32| unsafe {
            let moved = D2D_RECT_F {
                left: rect.left + dx,
                top: rect.top + dy,
                right: rect.right + dx,
                bottom: rect.bottom + dy,
            };
            target.rt.DrawText(
                text,
                format,
                &raw const moved,
                &target.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        };
        if let Some(mut shade) = shadow {
            // A 1 DIP ring of faint copies reads as a soft outline, plus a slightly
            // stronger copy below as the drop shadow.
            shade.a = 0.4 * color.a;
            unsafe { target.brush.SetColor(&raw const shade) };
            for (dx, dy) in SHADOW_RING {
                draw(dx, dy);
            }
            shade.a = 0.5 * color.a;
            unsafe { target.brush.SetColor(&raw const shade) };
            draw(0.0, 1.0);
        }
        unsafe { target.brush.SetColor(color) };
        draw(0.0, 0.0);
    }

    fn draw_status(&self, target: &Target, scene: &Scene) {
        let rect = scene.status_rect;
        if rect.right <= rect.left {
            return;
        }
        Self::draw_button_background(
            target,
            &scene.palette,
            rect,
            scene.plate_radius(rect),
            false,
            scene.status_pointer,
        );
        let shadow = scene.text_shadow();
        let mut text = scene.palette.text;
        text.a *= scene.status_reveal.alpha;
        let mut track_color = text;
        track_color.a *= 0.35;
        let mut left = rect.left + layout::STATUS_PADDING;
        for icon in &scene.status.icons {
            let cell = D2D_RECT_F {
                left,
                top: rect.top,
                right: left + layout::STATUS_CELL,
                bottom: rect.bottom,
            };
            if let Some(track) = icon.track {
                self.draw_text(
                    target,
                    &[track],
                    &self.icon_format,
                    cell,
                    &track_color,
                    None,
                );
            }
            self.draw_text(
                target,
                &[icon.glyph],
                &self.icon_format,
                cell,
                &text,
                shadow,
            );
            left += layout::STATUS_CELL;
        }
        if let Some(percent) = &scene.status.percent {
            let cell = D2D_RECT_F {
                // The island's layout measures the text: no extra space there.
                left: left + if scene.island.is_some() { 0.0 } else { 2.0 },
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            };
            // In the island everything but the main line is grey.
            let mut color = text;
            if scene.island.is_some() {
                color.a *= 0.6;
            }
            self.draw_text(target, percent, &self.percent_format, cell, &color, shadow);
        }
    }

    /// The strip's clock: the time (tabular, main) over the date (grey, smaller),
    /// as far from the panel's end as from its top and bottom.
    fn draw_island_clock(&self, target: &Target, scene: &Scene) {
        let rect = scene.clock_rect;
        let right = rect.right - layout::ISLAND_CLOCK_PADDING;
        let mid = (rect.top + rect.bottom) / 2.0;
        let line = |top: f32, bottom: f32| D2D_RECT_F {
            left: rect.left,
            top,
            right,
            bottom,
        };
        let mut main = scene.palette.text;
        main.a *= scene.clock_reveal.alpha;
        let mut grey = main;
        grey.a *= 0.6;
        if scene.clock.date.is_empty() {
            let r = line(mid - 9.0, mid + 9.0);
            self.draw_time(target, &scene.clock.time, &self.strip_time_format, r, &main);
        } else {
            let r = line(mid - 16.0, mid + 1.0);
            self.draw_time(target, &scene.clock.time, &self.strip_time_format, r, &main);
            let r = line(mid + 1.0, mid + 15.0);
            self.draw_time(target, &scene.clock.date, &self.strip_date_format, r, &grey);
        }
    }

    fn draw_clock(&self, target: &Target, scene: &Scene) {
        let rect = scene.clock_rect;
        Self::draw_button_background(
            target,
            &scene.palette,
            rect,
            scene.plate_radius(rect),
            false,
            scene.clock_pointer,
        );
        if scene.island.is_some() {
            self.draw_island_clock(target, scene);
            return;
        }
        let right = rect.right - layout::CLOCK_TEXT_PADDING;
        let left = rect.left;
        let mid = (rect.top + rect.bottom) / 2.0;
        let half = layout::CLOCK_LINE_HEIGHT / 2.0;
        let two_lines = [
            (&scene.clock.time, mid - layout::CLOCK_LINE_HEIGHT, mid),
            (&scene.clock.date, mid, mid + layout::CLOCK_LINE_HEIGHT),
        ];
        let one_line = [(&scene.clock.time, mid - half, mid + half)];
        let lines: &[(&Vec<u16>, f32, f32)] = if scene.clock.date.is_empty() {
            &one_line
        } else {
            &two_lines
        };
        let shadow = scene.text_shadow();
        for &(text, top, bottom) in lines {
            let line = D2D_RECT_F {
                left,
                top,
                right,
                bottom,
            };
            self.draw_text(
                target,
                text,
                &self.clock_format,
                line,
                &scene.palette.text,
                shadow,
            );
        }
    }
}

#[cfg(test)]
mod offscreen;
