//! Direct2D / DirectWrite drawing. All coordinates here are DIPs; the render
//! target's DPI is set to the monitor DPI so Direct2D does the scaling.
//!
//! With a translucent background (clear / blur / acrylic) pixels are cleared to
//! transparent and DWM composes what is behind the bar underneath; text then uses
//! grayscale antialiasing (ClearType needs an opaque background) and, over an
//! unknown wallpaper, a soft shadow.

use std::collections::HashMap;

use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_LAYER_PARAMETERS, D2D1_PRESENT_OPTIONS_NONE,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory,
    ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1Layer, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
    IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{BOOL, PCWSTR, Result, w};
use windows_numerics::Matrix3x2;

use crate::clock::ClockText;
use crate::config::Background;
use crate::icons::Icon;
use crate::layout::{self, Metrics};
use crate::status::StatusView;
use crate::theme::Palette;

const TRANSPARENT: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.0,
};
const CLOCK_FONT_SIZE: f32 = 12.0;
/// Island shadow: this many stacked, slightly growing translucent copies of the
/// panel, `SHADOW_STEP` DIPs apart, shifted down by `SHADOW_DROP`.
const SHADOW_LAYERS: u32 = 8;
const SHADOW_STEP: f32 = 1.5;
const SHADOW_DROP: f32 = 3.0;
const SHADOW_ALPHA: f32 = 0.045;
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
    /// Opacity of the bar content (icons fade in while expanding).
    pub content_opacity: f32,
    /// Soft shadow strength under the panel, 0..1.
    pub shadow: f32,
    pub rest: Option<RestView>,
    /// App icon inside the pill while an app goes in / comes out.
    pub badge: Option<Badge>,
}

/// What the pill shows at rest: the active app's icon (crossfading to a new one)
/// on the left, the time on the right.
pub struct RestView {
    pub opacity: f32,
    /// Icons with their opacity and box (bar space); two while crossfading.
    pub icons: Vec<(Icon, f32, D2D_RECT_F)>,
    pub clock: Option<(Vec<u16>, D2D_RECT_F)>,
    pub text: D2D1_COLOR_F,
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

    /// Shadow color for text and glyphs when the bar shows the wallpaper (clear or
    /// light blur): dark under light text, light under dark text.
    fn text_shadow(&self) -> Option<D2D1_COLOR_F> {
        if !matches!(self.backdrop, Background::Clear | Background::Blur) {
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
    rt: ID2D1HwndRenderTarget,
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
            Ok(Self {
                d2d,
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
                rt,
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
                    unsafe { t.rt.Resize(&raw const size)? };
                    t.size = size;
                }
                t
            }
            None => self.create_target(hwnd, size, dpi)?,
        };

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
        let opacity = match &scene.island {
            Some(island) => self.draw_island(&mut target, island, dpi, &mut used),
            None => 1.0,
        };
        if opacity > 0.0 {
            // While the island morphs, the bar content fades in, clipped to the panel.
            let layer = scene
                .island
                .as_ref()
                .filter(|_| opacity < 1.0)
                .map(|island| D2D1_LAYER_PARAMETERS {
                    contentBounds: island.panel,
                    maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
                    maskTransform: Matrix3x2::identity(),
                    opacity,
                    ..Default::default()
                });
            if let Some(params) = &layer {
                unsafe { target.rt.PushLayer(params, target.layer.as_ref()) };
            }
            self.draw_content(&mut target, scene, dpi, &mut used);
            if layer.is_some() {
                unsafe { target.rt.PopLayer() };
            }
        }
        target.bitmaps.retain(|id, _| used.contains(id));

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

    /// The island panel (pill, or the bar while morphing). Returns the opacity
    /// of the bar content to draw on top (0 while collapsed).
    fn draw_island(
        &self,
        target: &mut Target,
        island: &IslandView,
        dpi: f32,
        used: &mut Vec<u64>,
    ) -> f32 {
        let rounded = D2D1_ROUNDED_RECT {
            rect: island.panel,
            radiusX: island.radius,
            radiusY: island.radius,
        };
        unsafe {
            if island.shadow > 0.0 {
                // Stacked copies, biggest (faintest) first: summed alpha falls off
                // smoothly away from the panel.
                let color = D2D1_COLOR_F {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: SHADOW_ALPHA * island.shadow,
                };
                target.brush.SetColor(&raw const color);
                for i in (1..=SHADOW_LAYERS).rev() {
                    let grow = i as f32 * SHADOW_STEP;
                    let layer = D2D1_ROUNDED_RECT {
                        rect: D2D_RECT_F {
                            left: island.panel.left - grow,
                            top: island.panel.top - grow + SHADOW_DROP,
                            right: island.panel.right + grow,
                            bottom: island.panel.bottom + grow + SHADOW_DROP,
                        },
                        radiusX: island.radius + grow,
                        radiusY: island.radius + grow,
                    };
                    target
                        .rt
                        .FillRoundedRectangle(&raw const layer, &target.brush);
                }
            }
            if island.fill.a > 0.0 {
                target.brush.SetColor(&raw const island.fill);
                target
                    .rt
                    .FillRoundedRectangle(&raw const rounded, &target.brush);
            }
            if island.border.a > 0.0 {
                // Hairline inside the panel edge.
                let inner = D2D1_ROUNDED_RECT {
                    rect: D2D_RECT_F {
                        left: island.panel.left + 0.5,
                        top: island.panel.top + 0.5,
                        right: island.panel.right - 0.5,
                        bottom: island.panel.bottom - 0.5,
                    },
                    radiusX: (island.radius - 0.5).max(0.0),
                    radiusY: (island.radius - 0.5).max(0.0),
                };
                target.brush.SetColor(&raw const island.border);
                target
                    .rt
                    .DrawRoundedRectangle(&raw const inner, &target.brush, 1.0, None);
            }
        }
        if let Some(rest) = &island.rest
            && rest.opacity > 0.0
        {
            for (icon, alpha, rect) in &rest.icons {
                used.push(icon.id);
                if let Some(bitmap) = Self::icon_bitmap(target, icon) {
                    // Drawn at the icon's own pixel size, on whole pixels.
                    let box_px = ((rect.right - rect.left) * dpi / 96.0).round();
                    let px = box_px.min(icon.px as f32);
                    let at = layout::pixel_square(rect, px, 0.0, dpi);
                    unsafe {
                        target.rt.DrawBitmap(
                            &bitmap,
                            Some(&raw const at),
                            alpha * rest.opacity,
                            D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                            None,
                        );
                    }
                }
            }
            if let Some((text, rect)) = &rest.clock {
                let mut color = rest.text;
                color.a *= rest.opacity;
                self.draw_text(target, text, &self.clock_format, *rect, &color, None);
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
        island.content_opacity
    }

    /// Start, app buttons, status and clock.
    fn draw_content(&self, target: &mut Target, scene: &Scene, dpi: f32, used: &mut Vec<u64>) {
        let palette = &scene.palette;
        Self::draw_start(
            target,
            palette,
            &scene.metrics,
            scene.start.0,
            scene.start.1,
        );
        for button in &scene.buttons {
            Self::draw_button(
                target,
                palette,
                &scene.metrics,
                dpi,
                scene.text_shadow(),
                button,
                used,
            );
        }
        self.draw_status(target, scene);
        self.draw_clock(target, scene);
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
            Self::fill_rounded(target, rect, layout::CORNER_RADIUS, color);
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
        Self::draw_button_background(target, palette, rect, false, pointer);
        let size = if pointer.pressed {
            metrics.start_logo_size() * 0.85
        } else {
            metrics.start_logo_size()
        };
        let logo = layout::centered_square(&rect, size, 0.0);
        let gap = size * 0.06;
        let pane = (size - gap) / 2.0;
        let top_color = D2D1_COLOR_F {
            r: 0.30,
            g: 0.78,
            b: 1.0,
            a: 1.0,
        };
        let bottom_color = D2D1_COLOR_F {
            r: 0.0,
            g: 0.56,
            b: 0.94,
            a: 1.0,
        };
        for row in 0..2 {
            for col in 0..2 {
                let left = logo.left + col as f32 * (pane + gap);
                let top = logo.top + row as f32 * (pane + gap);
                let pane_rect = D2D_RECT_F {
                    left,
                    top,
                    right: left + pane,
                    bottom: top + pane,
                };
                let color = if row == 0 { &top_color } else { &bottom_color };
                Self::fill_rounded(target, pane_rect, size * 0.04, color);
            }
        }
    }

    fn icon_bitmap(target: &mut Target, icon: &Icon) -> Option<ID2D1Bitmap> {
        if let Some(bitmap) = target.bitmaps.get(&icon.id) {
            return Some(bitmap.clone());
        }
        let bitmap = unsafe { target.rt.CreateBitmapFromWicBitmap(&icon.bitmap, None) }.ok()?;
        target.bitmaps.insert(icon.id, bitmap.clone());
        Some(bitmap)
    }

    fn draw_button(
        target: &mut Target,
        palette: &Palette,
        metrics: &Metrics,
        dpi: f32,
        shadow: Option<D2D1_COLOR_F>,
        button: &ButtonView,
        used: &mut Vec<u64>,
    ) {
        let rect = button.rect;
        if button.state == ButtonState::Attention && !button.pointer.pressed {
            Self::fill_rounded(target, rect, layout::CORNER_RADIUS, &palette.attention_bg);
        } else {
            Self::draw_button_background(
                target,
                palette,
                rect,
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
        let wanted_px = (icon_size * dpi / 96.0).round();
        let icon_px = button
            .icon
            .map_or(wanted_px, |icon| wanted_px.min(icon.px as f32));
        let icon_rect = layout::pixel_square(&rect, icon_px, nudge, dpi);
        let bitmap = button.icon.and_then(|icon| {
            used.push(icon.id);
            Self::icon_bitmap(target, icon)
        });
        match bitmap {
            Some(bitmap) => unsafe {
                target.rt.DrawBitmap(
                    &bitmap,
                    Some(&raw const icon_rect),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            },
            None => Self::fill_rounded(target, icon_rect, 4.0, &palette.placeholder),
        }

        let (width, color) = match button.state {
            ButtonState::Pinned => return,
            ButtonState::Running => (layout::INDICATOR_RUNNING_WIDTH, &palette.indicator),
            ButtonState::Active => (layout::INDICATOR_ACTIVE_WIDTH, &palette.accent),
            ButtonState::Attention => (layout::INDICATOR_ACTIVE_WIDTH, &palette.attention),
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
        Self::fill_rounded(target, pill, layout::INDICATOR_HEIGHT / 2.0, color);
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
        Self::draw_button_background(target, &scene.palette, rect, false, scene.status_pointer);
        let shadow = scene.text_shadow();
        let text = scene.palette.text;
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
                left: left + 2.0,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            };
            self.draw_text(target, percent, &self.percent_format, cell, &text, shadow);
        }
    }

    fn draw_clock(&self, target: &Target, scene: &Scene) {
        let rect = scene.clock_rect;
        Self::draw_button_background(target, &scene.palette, rect, false, scene.clock_pointer);
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
