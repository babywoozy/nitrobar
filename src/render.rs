//! Direct2D / DirectWrite drawing. All coordinates here are DIPs; the render
//! target's DPI is set to the monitor DPI so Direct2D does the scaling.
//!
//! With the acrylic backdrop, pixels are cleared to transparent and DWM composes
//! the blurred backdrop underneath; text then uses grayscale antialiasing
//! (ClearType needs an opaque background).

use std::collections::HashMap;

use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
    D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT, DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
    IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{BOOL, PCWSTR, Result, w};

use crate::clock::ClockText;
use crate::icons::Icon;
use crate::layout;
use crate::status::StatusView;
use crate::theme::Palette;

const TRANSPARENT: D2D1_COLOR_F = D2D1_COLOR_F {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.0,
};
const CLOCK_FONT_SIZE: f32 = 12.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
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
    pub palette: Palette,
    /// True when DWM draws an acrylic backdrop under transparent pixels.
    pub translucent: bool,
    pub start: (D2D_RECT_F, Pointer),
    pub buttons: Vec<ButtonView<'a>>,
    pub status_rect: D2D_RECT_F,
    pub status_pointer: Pointer,
    pub status: StatusView,
    pub clock_rect: D2D_RECT_F,
    pub clock_pointer: Pointer,
    pub clock: ClockText,
}

/// Device-dependent resources, recreated when the device is lost.
struct Target {
    rt: ID2D1HwndRenderTarget,
    size: D2D_SIZE_U,
    brush: ID2D1SolidColorBrush,
    /// Icon bitmaps by `Icon::id`; pruned to the icons drawn in the last frame.
    bitmaps: HashMap<u64, ID2D1Bitmap>,
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
                rt,
                size,
                brush,
                bitmaps: HashMap::new(),
            })
        }
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
            target.rt.SetTextAntialiasMode(if scene.translucent {
                D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE
            } else {
                D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE
            });
            target.rt.BeginDraw();
            target.rt.Clear(Some(if scene.translucent {
                &TRANSPARENT
            } else {
                &palette.background
            }));
            if scene.translucent {
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

        Self::draw_start(&target, palette, scene.start.0, scene.start.1);
        let mut used = Vec::with_capacity(scene.buttons.len());
        for button in &scene.buttons {
            Self::draw_button(&mut target, palette, button, &mut used);
        }
        target.bitmaps.retain(|id, _| used.contains(id));
        self.draw_status(&target, scene);
        self.draw_clock(&target, scene);

        match unsafe { target.rt.EndDraw(None, None) } {
            // Device lost: drop the target, it is recreated on the next paint.
            Err(e) if e.code() == D2DERR_RECREATE_TARGET => Ok(()),
            Err(e) => Err(e),
            Ok(()) => {
                self.target = Some(target);
                Ok(())
            }
        }
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
    fn draw_start(target: &Target, palette: &Palette, rect: D2D_RECT_F, pointer: Pointer) {
        Self::draw_button_background(target, palette, rect, false, pointer);
        let size = if pointer.pressed {
            layout::START_LOGO_SIZE * 0.85
        } else {
            layout::START_LOGO_SIZE
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
            layout::ICON_SIZE_PRESSED
        } else {
            layout::ICON_SIZE
        };
        let icon_rect = layout::centered_square(&rect, icon_size, -1.0);
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
            ButtonState::Running => (layout::INDICATOR_RUNNING_WIDTH, &palette.indicator),
            ButtonState::Active => (layout::INDICATOR_ACTIVE_WIDTH, &palette.accent),
            ButtonState::Attention => (layout::INDICATOR_ACTIVE_WIDTH, &palette.attention),
        };
        let pill = layout::indicator(&rect, width);
        Self::fill_rounded(target, pill, layout::INDICATOR_HEIGHT / 2.0, color);
    }

    fn draw_text(
        &self,
        target: &Target,
        text: &[u16],
        format: &IDWriteTextFormat,
        rect: D2D_RECT_F,
    ) {
        unsafe {
            target.rt.DrawText(
                text,
                format,
                &raw const rect,
                &target.brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    }

    fn draw_status(&self, target: &Target, scene: &Scene) {
        let rect = scene.status_rect;
        if rect.right <= rect.left {
            return;
        }
        Self::draw_button_background(target, &scene.palette, rect, false, scene.status_pointer);
        let mut track_color = scene.palette.text;
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
                unsafe { target.brush.SetColor(&raw const track_color) };
                self.draw_text(target, &[track], &self.icon_format, cell);
            }
            unsafe { target.brush.SetColor(&raw const scene.palette.text) };
            self.draw_text(target, &[icon.glyph], &self.icon_format, cell);
            left += layout::STATUS_CELL;
        }
        if let Some(percent) = &scene.status.percent {
            let cell = D2D_RECT_F {
                left: left + 2.0,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            };
            self.draw_text(target, percent, &self.percent_format, cell);
        }
    }

    fn draw_clock(&self, target: &Target, scene: &Scene) {
        let rect = scene.clock_rect;
        Self::draw_button_background(target, &scene.palette, rect, false, scene.clock_pointer);
        let right = rect.right - layout::CLOCK_TEXT_PADDING;
        let left = rect.left;
        let mid = (rect.top + rect.bottom) / 2.0;
        let lines = [
            (&scene.clock.time, mid - layout::CLOCK_LINE_HEIGHT, mid),
            (&scene.clock.date, mid, mid + layout::CLOCK_LINE_HEIGHT),
        ];
        unsafe {
            target.brush.SetColor(&raw const scene.palette.text);
            for (text, top, bottom) in lines {
                let line = D2D_RECT_F {
                    left,
                    top,
                    right,
                    bottom,
                };
                target.rt.DrawText(
                    text,
                    &self.clock_format,
                    &raw const line,
                    &target.brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        }
    }
}
