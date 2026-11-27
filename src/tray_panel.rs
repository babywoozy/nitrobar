//! The island's tray panel: a small rounded panel above the strip with the apps'
//! tray icons. A layered, never activated popup showing a picture the renderer
//! drew; it reports the pointer to the bar through `WM_APP_PANEL`.

use std::cell::Cell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HGDIOBJ,
    SelectObject,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW,
    IDC_ARROW, LoadCursorW, PostMessageW, RegisterClassExW, SW_SHOWNOACTIVATE, SetWindowLongPtrW,
    ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WM_APP, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_RBUTTONUP, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, w};

/// Sent to the bar. wParam: one of the constants below; lParam: the icon's index
/// (`HOVER`: -1 for none).
pub const WM_APP_PANEL: u32 = WM_APP + 14;
pub const LEAVE: usize = 0;
pub const ENTER: usize = 1;
pub const HOVER: usize = 2;
pub const LEFT: usize = 3;
pub const DOUBLE: usize = 4;
pub const RIGHT: usize = 5;

const WM_MOUSELEAVE: u32 = 0x02A3;
const CLASS: PCWSTR = w!("NitrobarTrayPanel");

// Sizes in DIPs, as compact as Windows' own overflow panel.
pub const CELL: f32 = 40.0;
const PAD: f32 = 4.0;
/// Room around the boxes for their shadows.
pub const SHADOW: f32 = 12.0;
const TIP_HEIGHT: f32 = 30.0;
const TIP_GAP: f32 = 6.0;
const TIP_PADDING: f32 = 10.0;
const TIP_MAX: f32 = 360.0;
const MAX_COLUMNS: usize = 5;

/// Where everything is, in DIPs from the window's top left.
#[derive(Clone, Debug)]
pub struct PanelLayout {
    pub size: (f32, f32),
    pub panel: D2D_RECT_F,
    pub cells: Vec<D2D_RECT_F>,
    /// The small box naming the icon under the pointer, beyond the panel (away
    /// from the strip).
    pub tip: Option<D2D_RECT_F>,
}

/// A panel for `icons` icons. `tip`: the cell a name belongs to and the width of
/// its text. `below`: the name goes under the panel (bar at the top).
pub fn layout(icons: usize, tip: Option<(usize, f32)>, below: bool) -> PanelLayout {
    let columns = icons.clamp(1, MAX_COLUMNS);
    let rows = icons.max(1).div_ceil(columns);
    let width = columns as f32 * CELL + 2.0 * PAD;
    let height = rows as f32 * CELL + 2.0 * PAD;
    // The room for the name is always there, so the panel never moves up or down.
    let top = SHADOW + if below { 0.0 } else { TIP_HEIGHT + TIP_GAP };
    // Horizontally, relative to the panel's left edge first.
    let named = tip.map(|(cell, text)| {
        let w = (text + 2.0 * TIP_PADDING).min(TIP_MAX).ceil();
        let middle = PAD + ((cell % columns) as f32 + 0.5) * CELL;
        ((middle - w / 2.0).round(), w)
    });
    let leftmost = named.map_or(0.0, |(x, _)| x.min(0.0));
    let rightmost = named.map_or(width, |(x, w)| (x + w).max(width));
    let left = SHADOW - leftmost;
    let panel = D2D_RECT_F {
        left,
        top,
        right: left + width,
        bottom: top + height,
    };
    let cells = (0..icons)
        .map(|i| {
            let x = panel.left + PAD + (i % columns) as f32 * CELL;
            let y = panel.top + PAD + (i / columns) as f32 * CELL;
            D2D_RECT_F {
                left: x,
                top: y,
                right: x + CELL,
                bottom: y + CELL,
            }
        })
        .collect();
    let tip = named.map(|(x, w)| {
        let y = if below {
            panel.bottom + TIP_GAP
        } else {
            panel.top - TIP_GAP - TIP_HEIGHT
        };
        D2D_RECT_F {
            left: left + x,
            top: y,
            right: left + x + w,
            bottom: y + TIP_HEIGHT,
        }
    });
    PanelLayout {
        size: (
            rightmost - leftmost + 2.0 * SHADOW,
            height + TIP_HEIGHT + TIP_GAP + 2.0 * SHADOW,
        ),
        panel,
        cells,
        tip,
    }
}

/// What the window procedure needs.
struct State {
    bar: HWND,
    /// Cells in window pixels.
    cells: Vec<RECT>,
    hover: Cell<Option<usize>>,
    tracking: Cell<bool>,
}

pub struct TrayPanel {
    hwnd: HWND,
    state: *mut State,
}

impl TrayPanel {
    pub fn new(bar: HWND) -> Option<Self> {
        unsafe {
            let instance = GetModuleHandleW(None).ok()?.into();
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_DBLCLKS,
                lpfnWndProc: Some(panel_proc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).ok()?,
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&raw const class);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
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
            let state = Box::into_raw(Box::new(State {
                bar,
                cells: Vec::new(),
                hover: Cell::new(None),
                tracking: Cell::new(false),
            }));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
            Some(Self { hwnd, state })
        }
    }

    /// Puts `pixels` (premultiplied BGRA, `size` pixels) on screen with its top
    /// left at `at`; `cells` are the clickable areas in window pixels.
    pub fn present(&mut self, at: POINT, size: SIZE, pixels: &[u8], cells: Vec<RECT>) {
        if pixels.len() != (size.cx * size.cy * 4).max(0) as usize {
            return;
        }
        // SAFETY: `state` lives as long as the window, and window messages are
        // handled on this thread, not while this runs.
        unsafe { (*self.state).cells = cells };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size.cx,
                // Top-down.
                biHeight: -size.cy,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            let dc = CreateCompatibleDC(None);
            let mut bits = std::ptr::null_mut();
            if let Ok(bitmap) = CreateDIBSection(
                None,
                &raw const info,
                DIB_RGB_COLORS,
                &raw mut bits,
                None,
                0,
            ) {
                std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u8>(), pixels.len());
                let old = SelectObject(dc, HGDIOBJ(bitmap.0));
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                    ..Default::default()
                };
                let source = POINT::default();
                let _ = UpdateLayeredWindow(
                    self.hwnd,
                    None,
                    Some(&raw const at),
                    Some(&raw const size),
                    Some(dc),
                    Some(&raw const source),
                    COLORREF(0),
                    Some(&raw const blend),
                    ULW_ALPHA,
                );
                SelectObject(dc, old);
                let _ = DeleteObject(HGDIOBJ(bitmap.0));
            }
            let _ = DeleteDC(dc);
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
    }
}

impl Drop for TrayPanel {
    fn drop(&mut self) {
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
            drop(Box::from_raw(self.state));
        }
    }
}

extern "system" fn panel_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const State;
    // SAFETY: set right after creation and cleared before the box is freed.
    let Some(state) = (unsafe { ptr.as_ref() }) else {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    let post = |what: usize, value: isize| unsafe {
        let _ = PostMessageW(Some(state.bar), WM_APP_PANEL, WPARAM(what), LPARAM(value));
    };
    let cell = || {
        let x = i32::from(lparam.0 as u16 as i16);
        let y = i32::from((lparam.0 >> 16) as u16 as i16);
        state
            .cells
            .iter()
            .position(|c| x >= c.left && x < c.right && y >= c.top && y < c.bottom)
    };
    match msg {
        WM_MOUSEMOVE => {
            if !state.tracking.get() {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                state
                    .tracking
                    .set(unsafe { TrackMouseEvent(&raw mut track) }.is_ok());
                post(ENTER, 0);
            }
            let now = cell();
            if state.hover.replace(now) != now {
                post(HOVER, now.map_or(-1, |i| i as isize));
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            state.tracking.set(false);
            state.hover.set(None);
            post(HOVER, -1);
            post(LEAVE, 0);
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_RBUTTONUP => {
            if let Some(i) = cell() {
                let what = match msg {
                    WM_LBUTTONUP => LEFT,
                    WM_RBUTTONUP => RIGHT,
                    _ => DOUBLE,
                };
                post(what, i as isize);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_grid_with_the_name_above() {
        let two = layout(2, None, false);
        assert_eq!(two.panel.right - two.panel.left, 2.0 * CELL + 2.0 * PAD);
        assert_eq!(two.panel.bottom - two.panel.top, CELL + 2.0 * PAD);
        assert!(two.tip.is_none());
        // A long name widens the window, not the panel, which keeps its height.
        let named = layout(2, Some((1, 200.0)), false);
        let tip = named.tip.unwrap();
        assert!(tip.bottom < named.panel.top && tip.left >= SHADOW);
        assert!(tip.right <= named.size.0 - SHADOW);
        assert_eq!(named.panel.top, two.panel.top);
        let cell = named.cells[1];
        assert_eq!(tip.left + tip.right, cell.left + cell.right);
        let many = layout(7, Some((6, 40.0)), true);
        assert_eq!(many.cells[5].top, many.cells[0].bottom);
        assert!(many.tip.unwrap().top > many.panel.bottom);
        // Empty: still a panel, one cell large.
        assert!(layout(0, None, false).cells.is_empty());
    }
}
