//! nitrobar: a lightweight Windows 11 taskbar replacement.
//!
//! Command line:
//!   nitrobar                     start the bar
//!   nitrobar --hide-taskbar      start the bar and hide the native taskbar
//!   nitrobar --quit              close the running instance
//!   nitrobar --restore-taskbar   force the native taskbar back (recovery tool)

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Debug-only logging. Release builds have no console, so this compiles to nothing.
macro_rules! debug_log {
    ($($arg:tt)*) => {
        if cfg!(debug_assertions) {
            eprintln!($($arg)*);
        }
    };
}

mod app;
mod appbar;
mod clock;
mod config;
mod icons;
mod island;
mod layout;
mod menu;
mod pins;
mod render;
mod safety;
mod shell;
mod status;
mod taskbar;
mod tasks;
mod theme;
mod winpins;

use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

#[derive(Default)]
struct Args {
    hide_taskbar: bool,
    quit: bool,
    restore_taskbar: bool,
}

fn parse_args() -> Args {
    let mut args = Args::default();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--hide-taskbar" => args.hide_taskbar = true,
            "--quit" => args.quit = true,
            "--restore-taskbar" => args.restore_taskbar = true,
            other => debug_log!("nitrobar: ignoring unknown argument {other}"),
        }
    }
    args
}

fn main() {
    let args = parse_args();

    if args.quit {
        app::quit_running_instance();
        return;
    }
    if args.restore_taskbar {
        taskbar::force_restore();
        return;
    }
    if app::is_running() {
        debug_log!("nitrobar: already running");
        return;
    }

    // Must happen before any window is created. Failure only means we run DPI-unaware.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    safety::install();
    // A previous run may have been killed while the native taskbar was hidden.
    taskbar::recover_after_crash();

    let options = app::Options {
        hide_taskbar: args.hide_taskbar,
    };
    if let Err(err) = app::run(options) {
        debug_log!("nitrobar: fatal error: {err}");
        safety::emergency_cleanup();
        std::process::exit(1);
    }
    safety::emergency_cleanup();
}
