//! Window previews for an app with several windows: a small floating panel above
//! the island strip with a DWM thumbnail and the title of every window; clicking
//! one picks that window.
//!
//! The panel is a plain popup (never activated) painted with GDI: dark rounded
//! plate, one cell per window, hover highlight, titles. DWM draws the live
//! thumbnails on top of it. It tells the bar about the pointer (enter / leave,
//! so the strip stays open while the pointer is on the panel) and about the pick
//! through `WM_APP_PREVIEW`.

use std::cell::RefCell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_VISIBLE,
    DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmUnregisterThumbnail,
    DwmUpdateThumbnailProperties,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontIndirectW, CreateRoundRectRgn, CreateSolidBrush, DT_CENTER,
    DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint,
    FillRgn, HFONT, HGDIOBJ, InvalidateRect, LOGFONTW, PAINTSTRUCT, SelectObject, SetBkMode,
    SetTextColor, SetWindowRgn, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetWindowLongPtrW,
    GetWindowTextW, HWND_TOPMOST, IDC_ARROW, LoadCursorW, PostMessageW, RegisterClassExW,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetWindowLongPtrW, SetWindowPos, ShowWindow, WM_APP,
    WM_ERASEBKGND, WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WNDCLASSEXW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PCWSTR, w};

/// Sent to the bar. wParam: `LEAVE` / `ENTER` (pointer on the panel) or `PICK`
/// (lParam = the chosen window).
pub const WM_APP_PREVIEW: u32 = WM_APP + 10;
pub const LEAVE: usize = 0;
pub const ENTER: usize = 1;
pub const PICK: usize = 2;

/// Sent after `TrackMouseEvent(TME_LEAVE)` when the pointer leaves the window.
const WM_MOUSELEAVE: u32 = 0x02A3;

// Sizes in DIPs.
const CELL_WIDTH: f32 = 184.0;
const THUMB_HEIGHT: f32 = 108.0;
const TITLE_HEIGHT: f32 = 26.0;
const PAD: f32 = 10.0;
const GAP: f32 = 8.0;
const CELL_INSET: f32 = 6.0;
const RADIUS: f32 = 16.0;
const CELL_RADIUS: f32 = 10.0;
const MAX_COLUMNS: usize = 4;
const FONT_PT: f32 = 9.0;

const PLATE: COLORREF = COLORREF(0x001E1C1C);
const CELL: COLORREF = COLORREF(0x002D2B2B);
const CELL_HOVER: COLORREF = COLORREF(0x00464343);
const TEXT: COLORREF = COLORREF(0x00F0F0F0);

fn px(dip: f32, dpi: u32) -> i32 {
    (dip * dpi as f32 / 96.0).round() as i32
}

/// Cell rectangles (client pixels) for `n` windows and the panel's total size.
pub fn layout_cells(n: usize, dpi: u32) -> (Vec<RECT>, (i32, i32)) {
    let cols = n.clamp(1, MAX_COLUMNS);
    let rows = n.max(1).div_ceil(cols);
    let (cw, ch) = (px(CELL_WIDTH, dpi), px(THUMB_HEIGHT + TITLE_HEIGHT, dpi));
    let (pad, gap) = (px(PAD, dpi), px(GAP, dpi));
    let cells = (0..n)
        .map(|i| {
            let (col, row) = ((i % cols) as i32, (i / cols) as i32);
            let left = pad + col * (cw + gap);
            let top = pad + row * (ch + gap);
            RECT {
                left,
                top,
                right: left + cw,
                bottom: top + ch,
            }
        })
        .collect();
    let width = pad * 2 + cols as i32 * cw + (cols as i32 - 1) * gap;
    let height = pad * 2 + rows as i32 * ch + (rows as i32 - 1) * gap;
    (cells, (width, height))
}

struct Cell {
    target: HWND,
    rect: RECT,
    thumb: isize,
    title: Vec<u16>,
}

struct State {
    bar: HWND,
    cells: Vec<Cell>,
    hover: Option<usize>,
    tracking: bool,
    font: HFONT,
    dpi: u32,
}

pub struct Preview {
    hwnd: HWND,
    state: *mut RefCell<State>,
    /// Grouping key of the app this panel shows.
    pub key: String,
}

const CLASS: PCWSTR = w!("NitrobarPreview");

fn with_state<R>(hwnd: HWND, f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const RefCell<State>;
    let cell = unsafe { ptr.as_ref() }?;
    let mut state = cell.try_borrow_mut().ok()?;
    Some(f(&mut state))
}

fn cell_at(state: &State, x: i32, y: i32) -> Option<usize> {
    state
        .cells
        .iter()
        .position(|c| x >= c.rect.left && x < c.rect.right && y >= c.rect.top && y < c.rect.bottom)
}

fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    unsafe {
        let hdc = BeginPaint(hwnd, &raw mut ps);
        with_state(hwnd, |state| {
            let plate = CreateSolidBrush(PLATE);
            let cell = CreateSolidBrush(CELL);
            let hover = CreateSolidBrush(CELL_HOVER);
            let all = windows::Win32::Foundation::RECT {
                left: 0,
                top: 0,
                right: 100_000,
                bottom: 100_000,
            };
            windows::Win32::Graphics::Gdi::FillRect(hdc, &raw const all, plate);
            let radius = px(CELL_RADIUS, state.dpi) * 2;
            let old = SelectObject(hdc, HGDIOBJ(state.font.0));
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, TEXT);
            for (i, c) in state.cells.iter().enumerate() {
                let rgn = CreateRoundRectRgn(
                    c.rect.left,
                    c.rect.top,
                    c.rect.right + 1,
                    c.rect.bottom + 1,
                    radius,
                    radius,
                );
                let _ = FillRgn(hdc, rgn, if state.hover == Some(i) { hover } else { cell });
                let _ = DeleteObject(HGDIOBJ(rgn.0));
                let mut title = RECT {
                    left: c.rect.left + px(CELL_INSET, state.dpi),
                    top: c.rect.bottom - px(TITLE_HEIGHT, state.dpi),
                    right: c.rect.right - px(CELL_INSET, state.dpi),
                    bottom: c.rect.bottom - px(2.0, state.dpi),
                };
                let mut text = c.title.clone();
                DrawTextW(
                    hdc,
                    &mut text,
                    &raw mut title,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
                );
            }
            SelectObject(hdc, old);
            for brush in [plate, cell, hover] {
                let _ = DeleteObject(HGDIOBJ(brush.0));
            }
        });
        let _ = EndPaint(hwnd, &raw const ps);
    }
}

fn post_to_bar(hwnd: HWND, wparam: usize, lparam: isize) {
    if let Some(bar) = with_state(hwnd, |s| s.bar) {
        unsafe {
            let _ = PostMessageW(Some(bar), WM_APP_PREVIEW, WPARAM(wparam), LPARAM(lparam));
        }
    }
}

extern "system" fn preview_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            paint(hwnd);
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let x = i32::from(lparam.0 as u16 as i16);
            let y = i32::from((lparam.0 >> 16) as u16 as i16);
            let entered = with_state(hwnd, |state| {
                let first = !state.tracking;
                if first {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    state.tracking = unsafe { TrackMouseEvent(&raw mut track) }.is_ok();
                }
                let now = cell_at(state, x, y);
                if now != state.hover {
                    state.hover = now;
                    unsafe {
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }
                first
            });
            if entered == Some(true) {
                post_to_bar(hwnd, ENTER, 0);
            }
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with_state(hwnd, |state| {
                state.tracking = false;
                state.hover = None;
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            });
            post_to_bar(hwnd, LEAVE, 0);
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let x = i32::from(lparam.0 as u16 as i16);
            let y = i32::from((lparam.0 >> 16) as u16 as i16);
            let picked = with_state(hwnd, |state| {
                cell_at(state, x, y).map(|i| state.cells[i].target)
            })
            .flatten();
            if let Some(target) = picked {
                post_to_bar(hwnd, PICK, target.0 as isize);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn window_title(hwnd: HWND) -> Vec<u16> {
    let mut buffer = [0u16; 256];
    let len = unsafe { GetWindowTextW(hwnd, &mut buffer) }.max(0) as usize;
    buffer[..len].to_vec()
}

impl Preview {
    /// Shows the panel for `targets`, horizontally centered on `anchor_x` (screen
    /// pixels) and with its bottom (or, for a top bar, its top) edge at `edge_y`,
    /// kept inside `monitor`.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        bar: HWND,
        key: String,
        targets: &[HWND],
        anchor_x: i32,
        edge_y: i32,
        at_top: bool,
        monitor: &RECT,
        dpi: u32,
    ) -> Option<Self> {
        if targets.is_empty() {
            return None;
        }
        let (rects, (width, height)) = layout_cells(targets.len(), dpi);
        let left = (anchor_x - width / 2)
            .min(monitor.right - width - px(8.0, dpi))
            .max(monitor.left + px(8.0, dpi));
        let top = if at_top { edge_y } else { edge_y - height };
        unsafe {
            let instance = GetModuleHandleW(None).ok()?.into();
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(preview_proc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).ok()?,
                lpszClassName: CLASS,
                ..Default::default()
            };
            RegisterClassExW(&raw const class);
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                CLASS,
                w!(""),
                WS_POPUP,
                left,
                top,
                width,
                height,
                None,
                None,
                Some(instance),
                None,
            )
            .ok()?;
            crate::theme::no_border(hwnd);
            let mut face = [0u16; 32];
            for (dst, src) in face.iter_mut().zip("Segoe UI".encode_utf16()) {
                *dst = src;
            }
            let font = CreateFontIndirectW(&LOGFONTW {
                lfHeight: -((FONT_PT * dpi as f32 / 72.0).round() as i32),
                lfWeight: 400,
                lfQuality: windows::Win32::Graphics::Gdi::CLEARTYPE_QUALITY,
                lfFaceName: face,
                ..Default::default()
            });
            let cells: Vec<Cell> = targets
                .iter()
                .zip(rects)
                .filter_map(|(&target, rect)| {
                    let thumb = DwmRegisterThumbnail(hwnd, target).ok()?;
                    let inset = px(CELL_INSET, dpi);
                    let area = RECT {
                        left: rect.left + inset,
                        top: rect.top + inset,
                        right: rect.right - inset,
                        bottom: rect.top + px(THUMB_HEIGHT, dpi) - inset,
                    };
                    let source = DwmQueryThumbnailSourceSize(thumb).unwrap_or(SIZE {
                        cx: area.right - area.left,
                        cy: area.bottom - area.top,
                    });
                    let (aw, ah) = (area.right - area.left, area.bottom - area.top);
                    let fit = (aw as f32 / source.cx.max(1) as f32)
                        .min(ah as f32 / source.cy.max(1) as f32)
                        .min(2.0);
                    let (dw, dh) = (
                        (source.cx as f32 * fit).round() as i32,
                        (source.cy as f32 * fit).round() as i32,
                    );
                    let dest = RECT {
                        left: area.left + (aw - dw) / 2,
                        top: area.top + (ah - dh) / 2,
                        right: area.left + (aw - dw) / 2 + dw,
                        bottom: area.top + (ah - dh) / 2 + dh,
                    };
                    let props = DWM_THUMBNAIL_PROPERTIES {
                        dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_OPACITY | DWM_TNP_VISIBLE,
                        rcDestination: dest,
                        opacity: 255,
                        fVisible: true.into(),
                        ..Default::default()
                    };
                    let _ = DwmUpdateThumbnailProperties(thumb, &raw const props);
                    Some(Cell {
                        target,
                        rect,
                        thumb,
                        title: window_title(target),
                    })
                })
                .collect();
            let state = Box::into_raw(Box::new(RefCell::new(State {
                bar,
                cells,
                hover: None,
                tracking: false,
                font,
                dpi,
            })));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
            let radius = px(RADIUS, dpi) * 2;
            SetWindowRgn(
                hwnd,
                Some(CreateRoundRectRgn(
                    0,
                    0,
                    width + 1,
                    height + 1,
                    radius,
                    radius,
                )),
                false,
            );
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                left,
                top,
                width,
                height,
                SWP_NOACTIVATE,
            );
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Some(Self { hwnd, state, key })
        }
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let state = Box::from_raw(self.state);
            let state = state.into_inner();
            for cell in &state.cells {
                let _ = DwmUnregisterThumbnail(cell.thumb);
            }
            let _ = DeleteObject(HGDIOBJ(state.font.0));
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_fill_a_grid_inside_the_panel() {
        let (cells, (w, h)) = layout_cells(3, 96);
        assert_eq!(cells.len(), 3);
        assert!(cells.iter().all(|c| c.right <= w && c.bottom <= h));
        assert!(cells[0].right < cells[1].left, "gap between cells");
        // More windows than columns wrap to a second row.
        let (cells, (_, h2)) = layout_cells(MAX_COLUMNS + 1, 96);
        assert_eq!(cells[MAX_COLUMNS].left, cells[0].left);
        assert!(cells[MAX_COLUMNS].top > cells[0].bottom);
        assert!(h2 > h);
        // Scales with DPI.
        let (_, (w2, _)) = layout_cells(3, 192);
        assert_eq!(w2, w * 2);
    }
}
