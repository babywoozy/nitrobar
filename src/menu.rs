//! Right-click context menu.
//!
//! `TrackPopupMenu` runs a nested message loop, so it must never run while `App`
//! is borrowed: the caller gathers a `MenuContext`, releases the borrow, shows the
//! menu, then re-borrows to act on the choice.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, HMENU, MF_CHECKED, MF_GRAYED,
    MF_POPUP, MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow, TPM_BOTTOMALIGN,
    TPM_LEFTALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN, TrackPopupMenu,
    WM_NULL,
};
use windows::core::{HSTRING, PCWSTR, w};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    Pin,
    Unpin,
    CloseWindows,
    OpenConfig,
    Exit,
    /// "Màu island" submenu.
    ColorWallpaper,
    ColorAccent,
    ColorCustom,
}

const ALL: [Command; 8] = [
    Command::Pin,
    Command::Unpin,
    Command::CloseWindows,
    Command::OpenConfig,
    Command::Exit,
    Command::ColorWallpaper,
    Command::ColorAccent,
    Command::ColorCustom,
];

impl Command {
    fn id(self) -> usize {
        match self {
            Command::Pin => 1,
            Command::Unpin => 2,
            Command::CloseWindows => 3,
            Command::OpenConfig => 4,
            Command::Exit => 5,
            Command::ColorWallpaper => 6,
            Command::ColorAccent => 7,
            Command::ColorCustom => 8,
        }
    }
}

/// What was right-clicked.
#[derive(Default)]
pub struct MenuContext {
    /// Right-clicked an app button (pinned and/or running).
    pub on_app: bool,
    pub pinned: bool,
    pub window_count: usize,
    /// Last config.toml error, shown as a disabled item.
    pub config_error: Option<String>,
    /// Bar docked at the top: open the menu downwards.
    pub at_top: bool,
    /// Island with the glass style: offers the "Màu island" submenu, with the
    /// current source ticked.
    pub island_colors: Option<IslandColorChoice>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IslandColorChoice {
    Wallpaper,
    Accent,
    Custom,
}

/// Screen position for a WM_CONTEXTMENU lParam (-1,-1 means "from keyboard").
pub fn menu_point(lparam: LPARAM) -> POINT {
    let x = i32::from(lparam.0 as u16 as i16);
    let y = i32::from((lparam.0 >> 16) as u16 as i16);
    if x == -1 && y == -1 {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&raw mut pt);
        }
        pt
    } else {
        POINT { x, y }
    }
}

fn add(menu: HMENU, cmd: Command, label: PCWSTR) {
    unsafe {
        let _ = AppendMenuW(menu, MF_STRING, cmd.id(), label);
    }
}

fn separator(menu: HMENU) {
    unsafe {
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    }
}

/// Shows the menu at `at` (screen coordinates) and returns the chosen command.
pub fn show(owner: HWND, at: POINT, ctx: &MenuContext) -> Option<Command> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        if ctx.on_app {
            if ctx.pinned {
                add(menu, Command::Unpin, w!("Bỏ ghim khỏi thanh"));
            } else {
                add(menu, Command::Pin, w!("Ghim vào thanh"));
            }
            match ctx.window_count {
                0 => {}
                1 => add(menu, Command::CloseWindows, w!("Đóng cửa sổ")),
                _ => add(menu, Command::CloseWindows, w!("Đóng tất cả cửa sổ")),
            }
            separator(menu);
        }
        // Keep the HSTRING alive until the menu is destroyed.
        let error_label = ctx
            .config_error
            .as_ref()
            .map(|e| HSTRING::from(format!("⚠ Lỗi config.toml: {}", first_line(e))));
        if let Some(label) = &error_label {
            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, label);
        }
        if let Some(choice) = ctx.island_colors {
            let sub = CreatePopupMenu().ok()?;
            let checked = |c: IslandColorChoice| if choice == c { MF_CHECKED } else { MF_STRING };
            let _ = AppendMenuW(
                sub,
                MF_STRING | checked(IslandColorChoice::Wallpaper),
                Command::ColorWallpaper.id(),
                w!("Theo hình nền"),
            );
            let _ = AppendMenuW(
                sub,
                MF_STRING | checked(IslandColorChoice::Accent),
                Command::ColorAccent.id(),
                w!("Theo màu Windows"),
            );
            let _ = AppendMenuW(
                sub,
                MF_STRING | checked(IslandColorChoice::Custom),
                Command::ColorCustom.id(),
                w!("Tự chọn..."),
            );
            // The submenu is destroyed with its parent.
            let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("Màu island"));
            separator(menu);
        }
        add(menu, Command::OpenConfig, w!("Mở file cấu hình"));
        add(menu, Command::Exit, w!("Thoát nitrobar"));

        // Required so the menu closes when clicking elsewhere (see TrackPopupMenu docs).
        let _ = SetForegroundWindow(owner);
        let vertical = if ctx.at_top {
            TPM_TOPALIGN
        } else {
            TPM_BOTTOMALIGN
        };
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON | TPM_LEFTALIGN | vertical,
            at.x,
            at.y,
            None,
            owner,
            None,
        );
        let _ = PostMessageW(Some(owner), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        let id = usize::try_from(chosen.0).ok()?;
        ALL.into_iter().find(|cmd| cmd.id() == id)
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut short: String = line.chars().take(80).collect();
    if line.chars().count() > 80 {
        short.push('…');
    }
    short
}

/// The standard color picker, starting at `initial`. Runs a modal loop: call it
/// with `App` NOT borrowed. Returns `0x00BBGGRR`-free (r, g, b) bytes.
pub fn choose_color(owner: HWND, initial: (u8, u8, u8)) -> Option<(u8, u8, u8)> {
    use windows::Win32::Foundation::COLORREF;
    use windows::Win32::UI::Controls::Dialogs::{
        CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW, ChooseColorW,
    };
    let mut custom = [COLORREF(0x00FF_FFFF); 16];
    let mut dialog = CHOOSECOLORW {
        lStructSize: size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: owner,
        rgbResult: COLORREF(
            u32::from(initial.0) | u32::from(initial.1) << 8 | u32::from(initial.2) << 16,
        ),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_RGBINIT | CC_FULLOPEN,
        ..Default::default()
    };
    unsafe { ChooseColorW(&raw mut dialog) }.as_bool().then(|| {
        let c = dialog.rgbResult.0;
        (
            (c & 0xff) as u8,
            ((c >> 8) & 0xff) as u8,
            ((c >> 16) & 0xff) as u8,
        )
    })
}
