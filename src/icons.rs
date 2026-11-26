//! Real application icons, converted once into small premultiplied WIC bitmaps.
//!
//! Lookup order for a window:
//! 1. AppUserModelID -> `shell:AppsFolder\<AUMID>` item image (works for UWP / packaged
//!    apps, whose ApplicationFrameHost windows have no useful HICON).
//! 2. The window's own icon (WM_GETICON, then the class icon).
//! 3. The executable's shell icon.

use std::cell::Cell;

use windows::Win32::Foundation::{LPARAM, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{DeleteObject, HBITMAP, HPALETTE};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmap, IWICBitmapSource,
    IWICImagingFactory, WICBitmapCacheOnLoad, WICBitmapDitherTypeNone,
    WICBitmapInterpolationModeHighQualityCubic, WICBitmapPaletteTypeCustom,
    WICBitmapUsePremultipliedAlpha,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::{
    FOLDERID_AppsFolder, IShellItemImageFactory, KF_FLAG_DEFAULT, SHCreateItemFromParsingName,
    SHCreateItemInKnownFolder, SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GCLP_HICON, GCLP_HICONSM, GetClassLongPtrW, HICON, ICON_BIG, ICON_SMALL2, SMTO_ABORTIFHUNG,
    SMTO_BLOCK, SendMessageTimeoutW, WM_GETICON,
};
use windows::core::{HSTRING, Interface, Result};

use crate::tasks::AppId;
use windows::Win32::Foundation::HWND;

/// A decoded icon. `id` is unique per load so the renderer can cache GPU/CPU bitmaps.
pub struct Icon {
    pub id: u64,
    pub bitmap: IWICBitmap,
}

pub struct IconLoader {
    wic: IWICImagingFactory,
    next_id: Cell<u64>,
}

impl IconLoader {
    /// Requires COM to be initialized on the calling thread.
    pub fn new() -> Result<Self> {
        let wic =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)? };
        Ok(Self {
            wic,
            next_id: Cell::new(1),
        })
    }

    pub fn load(&self, hwnd: HWND, app: &AppId, px: u32) -> Option<Icon> {
        let bitmap = app
            .aumid
            .as_deref()
            .and_then(|aumid| self.apps_folder_bitmap(aumid, px))
            .or_else(|| self.window_bitmap(hwnd, px))
            .or_else(|| app.exe.as_deref().and_then(|exe| self.file_bitmap(exe, px)))?;
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        Some(Icon { id, bitmap })
    }

    /// Icon of an app that may not be running (pinned): AppsFolder entry, else the
    /// shortcut it was pinned through, else the exe.
    pub fn load_app(&self, app: &AppId, link: Option<&str>, px: u32) -> Option<Icon> {
        let bitmap = app
            .aumid
            .as_deref()
            .and_then(|aumid| self.apps_folder_bitmap(aumid, px))
            .or_else(|| link.and_then(|link| self.file_bitmap(link, px)))
            .or_else(|| app.exe.as_deref().and_then(|exe| self.file_bitmap(exe, px)))?;
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        Some(Icon { id, bitmap })
    }

    fn apps_folder_bitmap(&self, aumid: &str, px: u32) -> Option<IWICBitmap> {
        let factory: IShellItemImageFactory = unsafe {
            SHCreateItemInKnownFolder(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, &HSTRING::from(aumid))
        }
        .ok()?;
        self.image_factory_bitmap(&factory, px)
    }

    fn file_bitmap(&self, path: &str, px: u32) -> Option<IWICBitmap> {
        let factory: IShellItemImageFactory =
            unsafe { SHCreateItemFromParsingName(&HSTRING::from(path), None) }.ok()?;
        self.image_factory_bitmap(&factory, px)
    }

    fn image_factory_bitmap(
        &self,
        factory: &IShellItemImageFactory,
        px: u32,
    ) -> Option<IWICBitmap> {
        let size = SIZE {
            cx: px as i32,
            cy: px as i32,
        };
        let hbitmap: HBITMAP = unsafe { factory.GetImage(size, SIIGBF_ICONONLY) }.ok()?;
        let wic = unsafe {
            self.wic.CreateBitmapFromHBITMAP(
                hbitmap,
                HPALETTE::default(),
                WICBitmapUsePremultipliedAlpha,
            )
        };
        unsafe {
            let _ = DeleteObject(hbitmap.into());
        }
        self.normalize(&wic.ok()?.cast().ok()?, px)
    }

    fn window_bitmap(&self, hwnd: HWND, px: u32) -> Option<IWICBitmap> {
        let hicon = window_icon(hwnd)?;
        // The HICON belongs to the other process / class: copy it, never destroy it.
        let wic = unsafe { self.wic.CreateBitmapFromHICON(hicon) }.ok()?;
        self.normalize(&wic.cast().ok()?, px)
    }

    /// Converts to premultiplied BGRA, scales to `px`, and decodes into owned memory.
    fn normalize(&self, source: &IWICBitmapSource, px: u32) -> Option<IWICBitmap> {
        unsafe {
            let converter = self.wic.CreateFormatConverter().ok()?;
            converter
                .Initialize(
                    source,
                    &GUID_WICPixelFormat32bppPBGRA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
                .ok()?;
            let scaler = self.wic.CreateBitmapScaler().ok()?;
            scaler
                .Initialize(
                    &converter,
                    px,
                    px,
                    WICBitmapInterpolationModeHighQualityCubic,
                )
                .ok()?;
            self.wic
                .CreateBitmapFromSource(&scaler, WICBitmapCacheOnLoad)
                .ok()
        }
    }
}

fn window_icon(hwnd: HWND) -> Option<HICON> {
    for kind in [ICON_BIG, ICON_SMALL2] {
        let mut result = 0usize;
        // SMTO_BLOCK: don't dispatch other sent messages to us while waiting (re-entrancy).
        let ok = unsafe {
            SendMessageTimeoutW(
                hwnd,
                WM_GETICON,
                WPARAM(kind as usize),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                100,
                Some(&mut result),
            )
        };
        if ok.0 != 0 && result != 0 {
            return Some(HICON(result as *mut _));
        }
    }
    for index in [GCLP_HICON, GCLP_HICONSM] {
        let handle = unsafe { GetClassLongPtrW(hwnd, index) };
        if handle != 0 {
            return Some(HICON(handle as *mut _));
        }
    }
    None
}
