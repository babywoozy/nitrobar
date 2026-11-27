//! The picture of the tray panel, drawn into memory for its layered window.

use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_RENDER_TARGET_TYPE_SOFTWARE, D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
    WICBitmapCacheOnLoad,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};

use super::{Renderer, polygon};
use crate::shape;
use crate::tray::TrayIcon;
use crate::tray_panel::PanelLayout;

/// Corner reach of the panel's continuous corners, DIPs.
const CORNER: f32 = 20.0;
/// Largest an icon is drawn, DIPs.
const ICON: f32 = 20.0;

fn white(a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a,
    }
}

impl Renderer {
    /// The panel with `icons`, `hover` highlighted and named, as premultiplied
    /// BGRA of `layout.size` at `scale` device pixels per DIP.
    pub fn tray_panel(
        &self,
        layout: &PanelLayout,
        icons: &[TrayIcon],
        hover: Option<usize>,
        fill: D2D1_COLOR_F,
        scale: f32,
    ) -> Option<(u32, u32, Vec<u8>)> {
        let width = (layout.size.0 * scale).round() as u32;
        let height = (layout.size.1 * scale).round() as u32;
        unsafe {
            let wic: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
            let canvas = wic
                .CreateBitmap(
                    width,
                    height,
                    &GUID_WICPixelFormat32bppPBGRA,
                    WICBitmapCacheOnLoad,
                )
                .ok()?;
            let format = D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            };
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
                pixelFormat: format,
                dpiX: 96.0 * scale,
                dpiY: 96.0 * scale,
                ..Default::default()
            };
            let rt = self
                .d2d
                .CreateWicBitmapRenderTarget(&canvas, &raw const props)
                .ok()?;
            let brush = rt.CreateSolidColorBrush(&white(1.0), None).ok()?;
            rt.BeginDraw();
            rt.Clear(None);
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);

            let panel = layout.panel;
            let outline = |r: &D2D_RECT_F, reach: f32| {
                polygon(&self.d2d, &shape::outline(r, reach, reach, 2.8)).ok()
            };
            // Soft shadow: widening strokes of low alpha.
            if let Some(geometry) = outline(&panel, CORNER) {
                brush.SetColor(&D2D1_COLOR_F {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.03,
                });
                for step in 1..=8 {
                    rt.DrawGeometry(&geometry, &brush, step as f32 * 2.0, None);
                }
                brush.SetColor(&fill);
                rt.FillGeometry(&geometry, &brush, None);
            }
            for (i, (cell, icon)) in layout.cells.iter().zip(icons).enumerate() {
                if hover == Some(i)
                    && let Some(plate) = outline(
                        &D2D_RECT_F {
                            left: cell.left + 2.0,
                            top: cell.top + 2.0,
                            right: cell.right - 2.0,
                            bottom: cell.bottom - 2.0,
                        },
                        12.0,
                    )
                {
                    brush.SetColor(&white(0.12));
                    rt.FillGeometry(&plate, &brush, None);
                }
                let Some(image) = &icon.image else {
                    continue;
                };
                let props = D2D1_BITMAP_PROPERTIES {
                    pixelFormat: format,
                    dpiX: 96.0,
                    dpiY: 96.0,
                };
                let Ok(bitmap) = rt.CreateBitmap(
                    windows::Win32::Graphics::Direct2D::Common::D2D_SIZE_U {
                        width: image.width,
                        height: image.height,
                    },
                    Some(image.pixels.as_ptr().cast()),
                    image.width * 4,
                    &raw const props,
                ) else {
                    continue;
                };
                // Small pictures 1:1, big ones reduced; on whole device pixels.
                let side = (image.width as f32).min((ICON * scale).round());
                let snap = |v: f32| (v * scale - side / 2.0).round() / scale;
                let left = snap((cell.left + cell.right) / 2.0);
                let top = snap((cell.top + cell.bottom) / 2.0);
                let at = D2D_RECT_F {
                    left,
                    top,
                    right: left + side / scale,
                    bottom: top + side / scale,
                };
                rt.DrawBitmap(
                    &bitmap,
                    Some(&raw const at),
                    1.0,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            }
            // The hovered icon's name (first line of its tooltip).
            let empty: Vec<u16>;
            let name: &[u16] = if icons.is_empty() {
                empty = "Chưa có icon nào".encode_utf16().collect();
                &empty
            } else {
                hover.and_then(|i| icons.get(i)).map_or(&[][..], |icon| {
                    icon.tip
                        .split(|&c| c == u16::from(b'\n') || c == u16::from(b'\r'))
                        .next()
                        .unwrap_or_default()
                })
            };
            if !name.is_empty() {
                let room = layout.caption.right - layout.caption.left;
                let text = self.text_width(name, &self.sub_format).min(room);
                let left = layout.caption.left + (room - text) / 2.0;
                let line = D2D_RECT_F {
                    left,
                    right: left + text + 1.0,
                    ..layout.caption
                };
                brush.SetColor(&white(0.6));
                rt.DrawText(
                    name,
                    &self.sub_format,
                    &raw const line,
                    &brush,
                    windows::Win32::Graphics::Direct2D::D2D1_DRAW_TEXT_OPTIONS_CLIP,
                    windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
                );
            }
            rt.EndDraw(None, None).ok()?;
            let mut pixels = vec![0u8; (width * height * 4) as usize];
            canvas
                .CopyPixels(std::ptr::null(), width * 4, &mut pixels)
                .ok()?;
            Some((width, height, pixels))
        }
    }
}
