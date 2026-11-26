//! Real application icons, converted once into small premultiplied WIC bitmaps.
//!
//! Lookup order for a window:
//! 1. AppUserModelID -> `shell:AppsFolder\<AUMID>` item image (UWP / Store apps get
//!    their package logo; ApplicationFrameHost windows have no useful HICON).
//! 2. The executable's shell icon at the exact pixel size (icon resources carry
//!    hand-tuned 24 / 32 / 48 px images; scaling a 32 px window icon to 24 blurs it).
//! 3. The window's own icon (WM_GETICON, then the class icon), for host processes
//!    whose exe icon says nothing about the window (java, python, rundll32...).
//!
//! Icons are never scaled up: a 16 px-only icon stays 16 px, centered.

use std::cell::Cell;
use std::ffi::c_void;

use windows::Win32::Foundation::{LPARAM, PROPERTYKEY, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{DeleteObject, HBITMAP, HPALETTE};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, GUID_WICPixelFormat32bppPBGRA,
    IWICBitmap, IWICBitmapSource, IWICImagingFactory, WICBitmapCacheOnLoad,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeHighQualityCubic,
    WICBitmapPaletteTypeCustom, WICBitmapUseAlpha,
};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_RelaunchIconResource;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::Shell::SHDefExtractIconW;
use windows::Win32::UI::Shell::{
    FOLDERID_AppsFolder, IShellItemImageFactory, KF_FLAG_DEFAULT, SHCreateItemFromParsingName,
    SHCreateItemInKnownFolder, SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;
use windows::Win32::UI::WindowsAndMessaging::{
    GCLP_HICON, GCLP_HICONSM, GetClassLongPtrW, HICON, ICON_BIG, ICON_SMALL2, SMTO_ABORTIFHUNG,
    SMTO_BLOCK, SendMessageTimeoutW, WM_GETICON,
};
use windows::core::{HSTRING, Interface, Result};

use crate::tasks::AppId;
use windows::Win32::Foundation::HWND;

/// A decoded icon. `id` is unique per load so the renderer can cache bitmaps.
pub struct Icon {
    pub id: u64,
    pub bitmap: IWICBitmap,
    /// Width = height in pixels (at most the requested size; smaller icons are not enlarged).
    pub px: u32,
}

pub struct IconLoader {
    wic: IWICImagingFactory,
    next_id: Cell<u64>,
}

/// Processes whose exe icon is generic: use the window's icon instead.
const HOST_EXES: &[&str] = &[
    "applicationframehost.exe",
    "java.exe",
    "javaw.exe",
    "python.exe",
    "pythonw.exe",
    "rundll32.exe",
    "dllhost.exe",
    "mmc.exe",
    "wscript.exe",
    "powershell.exe",
    "pwsh.exe",
];

fn is_host_exe(exe: &str) -> bool {
    let name = exe.rsplit('\\').next().unwrap_or(exe).to_ascii_lowercase();
    HOST_EXES.contains(&name.as_str())
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

    fn finish(&self, bitmap: IWICBitmap) -> Option<Icon> {
        let (mut w, mut h) = (0, 0);
        unsafe { bitmap.GetSize(&raw mut w, &raw mut h) }.ok()?;
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        Some(Icon {
            id,
            bitmap,
            px: w.max(h),
        })
    }

    /// Icon for a running window of `app`.
    pub fn load(&self, hwnd: HWND, app: &AppId, px: u32) -> Option<Icon> {
        let exe = app.exe.as_deref().filter(|exe| !is_host_exe(exe));
        let bitmap = app
            .aumid
            .as_deref()
            .and_then(|aumid| self.apps_folder_bitmap(aumid, px))
            .or_else(|| self.relaunch_icon_bitmap(hwnd, px))
            .or_else(|| exe.and_then(|exe| self.file_bitmap(exe, px)))
            .or_else(|| self.window_bitmap(hwnd, px))
            .or_else(|| app.exe.as_deref().and_then(|exe| self.file_bitmap(exe, px)))?;
        self.finish(bitmap)
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
        self.finish(bitmap)
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

    /// The shell picks the icon image closest to `px` (exact-size resources win).
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
        let bitmap = self.bitmap_from_hbitmap(hbitmap);
        unsafe {
            let _ = DeleteObject(hbitmap.into());
        }
        self.normalize(&bitmap?.cast().ok()?, px)
    }

    /// Copies an HBITMAP from the shell, fixing up its alpha channel.
    ///
    /// The shell hands out 32 bpp DIBs whose alpha convention varies: usually
    /// straight (non-premultiplied) alpha, sometimes premultiplied, and for old
    /// icons without an alpha channel, all-zero alpha. Treating straight alpha as
    /// premultiplied lights up the transparent corners (bright fringes), so the
    /// pixels are inspected: any color above its alpha proves straight alpha.
    fn bitmap_from_hbitmap(&self, hbitmap: HBITMAP) -> Option<IWICBitmap> {
        unsafe {
            let raw = self
                .wic
                .CreateBitmapFromHBITMAP(hbitmap, HPALETTE::default(), WICBitmapUseAlpha)
                .ok()?;
            let (mut w, mut h) = (0u32, 0u32);
            raw.GetSize(&raw mut w, &raw mut h).ok()?;
            let stride = w * 4;
            let mut pixels = vec![0u8; (stride * h) as usize];
            raw.CopyPixels(std::ptr::null(), stride, &mut pixels).ok()?;
            let format = classify_alpha(&mut pixels);
            self.wic
                .CreateBitmapFromMemory(w, h, &format, stride, &pixels)
                .ok()
        }
    }

    /// `System.AppUserModel.RelaunchIconResource` ("path,index") set on the window,
    /// e.g. Chrome's per-profile icon with the profile badge. The taskbar uses it too.
    fn relaunch_icon_bitmap(&self, hwnd: HWND, px: u32) -> Option<IWICBitmap> {
        let resource = window_string_property(hwnd, &PKEY_AppUserModel_RelaunchIconResource)?;
        let (path, index) = match resource.rsplit_once(',') {
            Some((path, index)) => (path, index.trim().parse().ok()?),
            None => (resource.as_str(), 0),
        };
        let mut hicon = HICON::default();
        unsafe {
            SHDefExtractIconW(
                &HSTRING::from(path.trim()),
                index,
                0,
                Some(&raw mut hicon),
                None,
                px & 0xFFFF,
            )
            .ok()
            .ok()?;
        }
        if hicon.is_invalid() {
            return None;
        }
        let wic = unsafe { self.wic.CreateBitmapFromHICON(hicon) };
        unsafe {
            let _ = DestroyIcon(hicon);
        }
        self.normalize(&wic.ok()?.cast().ok()?, px)
    }

    fn window_bitmap(&self, hwnd: HWND, px: u32) -> Option<IWICBitmap> {
        let hicon = window_icon(hwnd)?;
        // The HICON belongs to the other process / class: copy it, never destroy it.
        // CreateBitmapFromHICON applies the icon mask and yields straight-alpha BGRA.
        let wic = unsafe { self.wic.CreateBitmapFromHICON(hicon) }.ok()?;
        self.normalize(&wic.cast().ok()?, px)
    }

    /// Converts to premultiplied BGRA, scales *down* to `px` with a high-quality
    /// filter (never up), and decodes into owned memory.
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
            let (mut w, mut h) = (0u32, 0u32);
            source.GetSize(&raw mut w, &raw mut h).ok()?;
            if w <= px && h <= px {
                return self
                    .wic
                    .CreateBitmapFromSource(&converter, WICBitmapCacheOnLoad)
                    .ok();
            }
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

/// Decides the pixel format of shell BGRA pixels (see `bitmap_from_hbitmap`);
/// images without any alpha are made opaque.
fn classify_alpha(pixels: &mut [u8]) -> windows::core::GUID {
    let mut any_alpha = false;
    let mut straight = false;
    for p in pixels.as_chunks::<4>().0 {
        let a = p[3];
        any_alpha |= a != 0;
        straight |= p[0] > a || p[1] > a || p[2] > a;
    }
    if !any_alpha {
        // No alpha channel at all (24-bit icon): fully opaque.
        for p in pixels.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
        return GUID_WICPixelFormat32bppPBGRA;
    }
    if straight {
        GUID_WICPixelFormat32bppBGRA
    } else {
        GUID_WICPixelFormat32bppPBGRA
    }
}

fn window_string_property(hwnd: HWND, key: &PROPERTYKEY) -> Option<String> {
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let value = store.GetValue(key).ok()?;
        let text = PropVariantToStringAlloc(&value).ok()?;
        let result = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const c_void));
        result.filter(|s| !s.is_empty())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_alpha_conventions() {
        // Straight: a half-transparent white pixel (color 255 > alpha 128).
        let mut straight = vec![255, 255, 255, 128, 0, 0, 0, 0];
        assert_eq!(classify_alpha(&mut straight), GUID_WICPixelFormat32bppBGRA);
        // Premultiplied: colors never exceed alpha.
        let mut pre = vec![100, 100, 100, 128, 0, 0, 0, 0];
        assert_eq!(classify_alpha(&mut pre), GUID_WICPixelFormat32bppPBGRA);
        // No alpha channel: made opaque.
        let mut none = vec![10, 20, 30, 0];
        assert_eq!(classify_alpha(&mut none), GUID_WICPixelFormat32bppPBGRA);
        assert_eq!(none[3], 255);
    }
}
