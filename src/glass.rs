//! Frosted glass behind the island: a real blur of whatever is behind the pill, with
//! corners that follow the pill's shape.
//!
//! `Windows.UI.Composition` `CreateHostBackdropBrush` blurs the desktop behind a
//! window (measured: windows behind it too, and for a window that is never
//! active). Composition output is drawn OVER the window's own GDI / Direct2D
//! surface though, so the glass lives in its own window right below the bar:
//!
//! - `WS_EX_NOREDIRECTIONBITMAP`: no surface of its own, transparent outside the
//!   composition visual; click-through; never activated.
//! - one `SpriteVisual` with the backdrop brush, clipped by a
//!   `CompositionRoundedRectangleGeometry` (anti-aliased, so the blur ends exactly
//!   at the rounded corners).
//!
//! The bar window above it draws the tint, hairline border, icons and text.

use windows::System::DispatcherQueueController;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionRoundedRectangleGeometry, Compositor, ContainerVisual, SpriteVisual,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassExW, SW_HIDE, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SetWindowPos, ShowWindow, WNDCLASSEXW, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{Interface, PCWSTR, w};
use windows_numerics::{Vector2, Vector3};

const CLASS: PCWSTR = w!("NitrobarGlass");
/// `DWMWA_USE_HOSTBACKDROPBRUSH`.
const USE_HOSTBACKDROPBRUSH: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(17);

extern "system" fn glass_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

pub struct Glass {
    hwnd: HWND,
    // Kept alive for as long as the glass exists.
    _controller: DispatcherQueueController,
    _compositor: Compositor,
    _target: DesktopWindowTarget,
    _root: ContainerVisual,
    visual: SpriteVisual,
    geometry: CompositionRoundedRectangleGeometry,
}

impl Glass {
    /// `None` when Windows.UI.Composition is unavailable (the caller then draws a
    /// plain translucent pill).
    pub fn new() -> Option<Self> {
        unsafe {
            let instance = GetModuleHandleW(None).ok()?.into();
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(glass_proc),
                hInstance: instance,
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&raw const class);
            let hwnd = CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOPMOST
                    | WS_EX_TRANSPARENT,
                CLASS,
                w!(""),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
            .ok()?;
            match Self::compose(hwnd) {
                Some(glass) => Some(glass),
                None => {
                    let _ = DestroyWindow(hwnd);
                    None
                }
            }
        }
    }

    unsafe fn compose(hwnd: HWND) -> Option<Self> {
        unsafe {
            let on: i32 = 1;
            let _ = DwmSetWindowAttribute(hwnd, USE_HOSTBACKDROPBRUSH, (&raw const on).cast(), 4);
            let options = DispatcherQueueOptions {
                dwSize: size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_NONE,
            };
            let controller = CreateDispatcherQueueController(options).ok()?;
            let compositor = Compositor::new().ok()?;
            let interop: ICompositorDesktopInterop = compositor.cast().ok()?;
            let target = interop.CreateDesktopWindowTarget(hwnd, false).ok()?;
            let root = compositor.CreateContainerVisual().ok()?;
            target.SetRoot(&root).ok()?;
            let visual = compositor.CreateSpriteVisual().ok()?;
            visual
                .SetBrush(&compositor.CreateHostBackdropBrush().ok()?)
                .ok()?;
            let geometry = compositor.CreateRoundedRectangleGeometry().ok()?;
            let clip = compositor.CreateGeometricClip().ok()?;
            clip.SetGeometry(&geometry).ok()?;
            visual.SetClip(&clip).ok()?;
            root.Children().ok()?.InsertAtTop(&visual).ok()?;
            Some(Self {
                hwnd,
                _controller: controller,
                _compositor: compositor,
                _target: target,
                _root: root,
                visual,
                geometry,
            })
        }
    }

    /// Covers `rc` (screen pixels) right below `above`, the bar window; or hides.
    pub fn place(&self, rc: &RECT, above: HWND, visible: bool) {
        unsafe {
            if !visible {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
                return;
            }
            let _ = SetWindowPos(
                self.hwnd,
                Some(above),
                rc.left,
                rc.top,
                rc.right - rc.left,
                rc.bottom - rc.top,
                SWP_NOACTIVATE,
            );
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }

    /// The blurred shape inside the window, in window pixels.
    pub fn set_shape(&self, x: f32, y: f32, w: f32, h: f32, radius: f32) {
        let _ = self.visual.SetOffset(Vector3 { X: x, Y: y, Z: 0.0 });
        let _ = self.visual.SetSize(Vector2 { X: w, Y: h });
        let _ = self.geometry.SetSize(Vector2 { X: w, Y: h });
        let _ = self.geometry.SetCornerRadius(Vector2 {
            X: radius,
            Y: radius,
        });
    }
}

impl Drop for Glass {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}
