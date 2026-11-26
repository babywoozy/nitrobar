//! Direct2D / DirectWrite drawing. All coordinates here are DIPs; the render
//! target's DPI is set to the monitor DPI so Direct2D does the scaling.

use std::collections::HashMap;

use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_SOFTWARE,
    D2D1_ROUNDED_RECT, D2D1CreateFactory, ID2D1Bitmap, ID2D1Factory, ID2D1HwndRenderTarget,
    ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_WORD_WRAPPING_NO_WRAP, DWriteCreateFactory,
    IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{Result, w};

use crate::clock::ClockText;
use crate::icons::Icon;
use crate::layout;

const fn rgba(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}

// Windows 11 dark taskbar palette.
const BACKGROUND: D2D1_COLOR_F = rgba(0x1c, 0x1c, 0x1c, 1.0);
const FOREGROUND: D2D1_COLOR_F = rgba(0xff, 0xff, 0xff, 1.0);
const BUTTON_ACTIVE_BG: D2D1_COLOR_F = rgba(0xff, 0xff, 0xff, 0.06);
const BUTTON_ATTENTION_BG: D2D1_COLOR_F = rgba(0xff, 0x8c, 0x00, 0.28);
const INDICATOR_RUNNING: D2D1_COLOR_F = rgba(0xff, 0xff, 0xff, 0.55);
const INDICATOR_ACTIVE: D2D1_COLOR_F = rgba(0x4c, 0xc2, 0xff, 1.0);
const INDICATOR_ATTENTION: D2D1_COLOR_F = rgba(0xff, 0x8c, 0x00, 1.0);
const PLACEHOLDER_ICON: D2D1_COLOR_F = rgba(0xff, 0xff, 0xff, 0.25);

const CLOCK_FONT_SIZE: f32 = 12.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonState {
    Running,
    Active,
    Attention,
}

pub struct ButtonView<'a> {
    pub rect: D2D_RECT_F,
    pub icon: Option<&'a Icon>,
    pub state: ButtonState,
}

pub struct Scene<'a> {
    pub buttons: Vec<ButtonView<'a>>,
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
    target: Option<Target>,
}

impl Renderer {
    pub fn new() -> Result<Self> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let clock_format = dwrite.CreateTextFormat(
                w!("Segoe UI Variable Text"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                CLOCK_FONT_SIZE,
                w!(""),
            )?;
            clock_format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
            clock_format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            clock_format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            Ok(Self {
                d2d,
                clock_format,
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
            let brush = rt.CreateSolidColorBrush(&FOREGROUND, None)?;
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

        unsafe {
            target.rt.SetDpi(dpi, dpi);
            target.rt.BeginDraw();
            target.rt.Clear(Some(&BACKGROUND));
        }
        let mut used = Vec::with_capacity(scene.buttons.len());
        for button in &scene.buttons {
            Self::draw_button(&mut target, button, &mut used);
        }
        target.bitmaps.retain(|id, _| used.contains(id));
        self.draw_clock(&target, &scene.clock);

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

    fn draw_button(target: &mut Target, button: &ButtonView, used: &mut Vec<u64>) {
        let rect = button.rect;
        match button.state {
            ButtonState::Active => {
                Self::fill_rounded(target, rect, layout::CORNER_RADIUS, &BUTTON_ACTIVE_BG);
            }
            ButtonState::Attention => {
                Self::fill_rounded(target, rect, layout::CORNER_RADIUS, &BUTTON_ATTENTION_BG);
            }
            ButtonState::Running => {}
        }

        let icon_rect = layout::centered_square(&rect, layout::ICON_SIZE, -1.0);
        let bitmap = button.icon.and_then(|icon| {
            used.push(icon.id);
            if let Some(bitmap) = target.bitmaps.get(&icon.id) {
                return Some(bitmap.clone());
            }
            let bitmap = unsafe { target.rt.CreateBitmapFromWicBitmap(&icon.bitmap, None) }.ok()?;
            target.bitmaps.insert(icon.id, bitmap.clone());
            Some(bitmap)
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
            None => Self::fill_rounded(target, icon_rect, 4.0, &PLACEHOLDER_ICON),
        }

        let (width, color) = match button.state {
            ButtonState::Running => (layout::INDICATOR_RUNNING_WIDTH, &INDICATOR_RUNNING),
            ButtonState::Active => (layout::INDICATOR_ACTIVE_WIDTH, &INDICATOR_ACTIVE),
            ButtonState::Attention => (layout::INDICATOR_ACTIVE_WIDTH, &INDICATOR_ATTENTION),
        };
        let pill = layout::indicator(&rect, width);
        Self::fill_rounded(target, pill, layout::INDICATOR_HEIGHT / 2.0, color);
    }

    fn draw_clock(&self, target: &Target, clock: &ClockText) {
        unsafe {
            let bar = target.rt.GetSize();
            let right = bar.width - layout::CLOCK_RIGHT_PADDING;
            let left = right - layout::CLOCK_WIDTH;
            let mid = bar.height / 2.0;
            let lines = [
                (&clock.time, mid - layout::CLOCK_LINE_HEIGHT, mid),
                (&clock.date, mid, mid + layout::CLOCK_LINE_HEIGHT),
            ];
            target.brush.SetColor(&FOREGROUND);
            for (text, top, bottom) in lines {
                let rect = D2D_RECT_F {
                    left,
                    top,
                    right,
                    bottom,
                };
                target.rt.DrawText(
                    text,
                    &self.clock_format,
                    &raw const rect,
                    &target.brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
        }
    }
}
