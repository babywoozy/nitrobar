//! Direct2D / DirectWrite drawing. All coordinates here are DIPs; the render
//! target's DPI is set to the monitor DPI so Direct2D does the scaling.

use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget,
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

const fn rgba(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a,
    }
}

// Windows 11 dark taskbar without Mica.
const BACKGROUND: D2D1_COLOR_F = rgba(0x1c, 0x1c, 0x1c, 1.0);
const FOREGROUND: D2D1_COLOR_F = rgba(0xff, 0xff, 0xff, 1.0);

const CLOCK_FONT_SIZE: f32 = 12.0;
const CLOCK_WIDTH: f32 = 120.0;
const CLOCK_RIGHT_PADDING: f32 = 12.0;
const CLOCK_LINE_HEIGHT: f32 = 16.0;

/// Device-dependent resources, recreated when the GPU device is lost.
struct Target {
    rt: ID2D1HwndRenderTarget,
    size: D2D_SIZE_U,
    text_brush: ID2D1SolidColorBrush,
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
        unsafe { GetClientRect(hwnd, &mut rc)? };
        Ok(D2D_SIZE_U {
            width: (rc.right - rc.left).max(0) as u32,
            height: (rc.bottom - rc.top).max(0) as u32,
        })
    }

    fn create_target(&self, hwnd: HWND, size: D2D_SIZE_U, dpi: f32) -> Result<Target> {
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            // A GPU target pulls in a D3D11 device + driver (~25 MB private memory).
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
            let rt = self.d2d.CreateHwndRenderTarget(&props, &hwnd_props)?;
            let text_brush = rt.CreateSolidColorBrush(&FOREGROUND, None)?;
            Ok(Target {
                rt,
                size,
                text_brush,
            })
        }
    }

    /// Draws the whole bar. Call between `BeginPaint` and `EndPaint`.
    pub fn draw(&mut self, hwnd: HWND, dpi: u32, clock: &ClockText) -> Result<()> {
        let size = Self::client_size(hwnd)?;
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let dpi = dpi as f32;

        let target = match self.target.take() {
            Some(mut t) => {
                if t.size.width != size.width || t.size.height != size.height {
                    unsafe { t.rt.Resize(&size)? };
                    t.size = size;
                }
                t
            }
            None => self.create_target(hwnd, size, dpi)?,
        };

        let result = unsafe {
            let rt = &target.rt;
            rt.SetDpi(dpi, dpi);
            rt.BeginDraw();
            rt.Clear(Some(&BACKGROUND));

            let bar = rt.GetSize();
            let right = bar.width - CLOCK_RIGHT_PADDING;
            let left = right - CLOCK_WIDTH;
            let mid = bar.height / 2.0;
            let time_rect = D2D_RECT_F {
                left,
                top: mid - CLOCK_LINE_HEIGHT,
                right,
                bottom: mid,
            };
            let date_rect = D2D_RECT_F {
                left,
                top: mid,
                right,
                bottom: mid + CLOCK_LINE_HEIGHT,
            };
            for (text, rect) in [(&clock.time, &time_rect), (&clock.date, &date_rect)] {
                rt.DrawText(
                    text,
                    &self.clock_format,
                    rect,
                    &target.text_brush,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            rt.EndDraw(None, None)
        };

        match result {
            // Device lost: drop the target, it is recreated on the next paint.
            Err(e) if e.code() == D2DERR_RECREATE_TARGET => Ok(()),
            Err(e) => Err(e),
            Ok(()) => {
                self.target = Some(target);
                Ok(())
            }
        }
    }
}
