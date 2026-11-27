//! Design screenshots without touching the screen: draws the island's states with
//! the real renderer into a bitmap and writes magnified BMP files.
//!
//! `NB_SHOTS=<folder> cargo test --release render_island_states -- --ignored`

use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmap, IWICImagingFactory,
    WICBitmapCacheOnLoad,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
};

use super::*;
use crate::config::BarConfig;
use crate::island;
use crate::status::StatusIcon;
use crate::theme::Theme;

const DPI: f32 = 96.0;
const ZOOM: usize = 5;

fn color(r: f32, g: f32, b: f32, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn palette() -> Palette {
    let accent = color(0.45, 0.95, 0.66, 1.0);
    let theme = Theme {
        light: false,
        backdrop: Background::Clear,
        accent,
    };
    let mut p = theme.palette();
    p.text = color(1.0, 1.0, 1.0, 0.97);
    p.accent = accent;
    p.indicator = color(1.0, 1.0, 1.0, 0.45);
    p.placeholder = color(1.0, 1.0, 1.0, 0.18);
    p.hover = color(1.0, 1.0, 1.0, 0.10);
    p
}

/// A round pink-to-blue stand-in cover, premultiplied BGRA.
fn cover(wic: &IWICImagingFactory, size: u32) -> Icon {
    let r = size as f32 / 2.0;
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let t = (x + y) as f32 / (2 * size) as f32;
            let d = ((x as f32 + 0.5 - r).powi(2) + (y as f32 + 0.5 - r).powi(2)).sqrt();
            let a = (r - d + 0.5).clamp(0.0, 1.0);
            let mix = |a0: f32, b0: f32| ((a0 + (b0 - a0) * t) * a) as u8;
            pixels.extend([mix(150.0, 240.0), mix(120.0, 110.0), mix(255.0, 70.0)]);
            pixels.push((a * 255.0) as u8);
        }
    }
    let bitmap = unsafe {
        wic.CreateBitmapFromMemory(
            size,
            size,
            &GUID_WICPixelFormat32bppPBGRA,
            size * 4,
            &pixels,
        )
    }
    .unwrap();
    Icon {
        id: 1,
        small: bitmap.clone(),
        small_px: size,
        large: bitmap.clone(),
        bitmap,
        px: size,
    }
}

/// Writes premultiplied BGRA composed over a flat background, magnified.
fn write_bmp(path: &std::path::Path, pixels: &[u8], w: usize, h: usize) {
    let background = [232.0f32, 190.0, 160.0]; // BGR
    let (zw, zh) = (w * ZOOM, h * ZOOM);
    let mut out = Vec::with_capacity(54 + zw * zh * 4);
    out.extend(b"BM");
    out.extend(((54 + zw * zh * 4) as u32).to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(54u32.to_le_bytes());
    out.extend(40u32.to_le_bytes());
    out.extend((zw as i32).to_le_bytes());
    out.extend((-(zh as i32)).to_le_bytes()); // top-down
    out.extend(1u16.to_le_bytes());
    out.extend(32u16.to_le_bytes());
    out.extend([0u8; 24]);
    for y in 0..zh {
        for x in 0..zw {
            let p = &pixels[((y / ZOOM) * w + x / ZOOM) * 4..][..4];
            let keep = 1.0 - f32::from(p[3]) / 255.0;
            for c in 0..3 {
                out.push((f32::from(p[c]) + background[c] * keep).min(255.0) as u8);
            }
            out.push(255);
        }
    }
    std::fs::write(path, out).unwrap();
}

fn offscreen_target(
    renderer: &Renderer,
    wic: &IWICImagingFactory,
    (w, h): (u32, u32),
) -> (IWICBitmap, Target) {
    unsafe {
        let bitmap = wic
            .CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)
            .unwrap();
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: DPI,
            dpiY: DPI,
            ..Default::default()
        };
        let rt = renderer
            .d2d
            .CreateWicBitmapRenderTarget(&bitmap, &raw const props)
            .unwrap();
        let brush = rt.CreateSolidColorBrush(&TRANSPARENT, None).unwrap();
        let target = Target {
            layer: rt.CreateLayer(None).ok(),
            window: None,
            rt,
            size: D2D_SIZE_U {
                width: w,
                height: h,
            },
            brush,
            bitmaps: HashMap::new(),
        };
        (bitmap, target)
    }
}

fn shot(
    renderer: &Renderer,
    wic: &IWICImagingFactory,
    scene: &Scene,
    size: (u32, u32),
    name: &str,
) {
    let dir = std::path::PathBuf::from(std::env::var("NB_SHOTS").expect("NB_SHOTS"));
    let (w, h) = size;
    unsafe {
        let (bitmap, mut target) = offscreen_target(renderer, wic, size);
        renderer.render(&mut target, scene, DPI);
        target.rt.EndDraw(None, None).unwrap();
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        bitmap
            .CopyPixels(std::ptr::null(), w * 4, &mut pixels)
            .unwrap();
        write_bmp(
            &dir.join(format!("{name}.bmp")),
            &pixels,
            w as usize,
            h as usize,
        );
    }
}

/// Draws `scene` `rounds` times into one target (what an animation frame costs).
fn measure(
    renderer: &Renderer,
    wic: &IWICImagingFactory,
    scene: &Scene,
    size: (u32, u32),
    rounds: u32,
) {
    let mut target = offscreen_target(renderer, wic, size).1;
    for _ in 0..rounds {
        renderer.render(&mut target, scene, DPI);
        unsafe { target.rt.EndDraw(None, None) }.unwrap();
    }
}

/// Distance in pixels from the panel's left / right end to the first / last
/// drawn pixel of the content (anything clearly brighter than the black panel).
fn end_margins(
    renderer: &Renderer,
    wic: &IWICImagingFactory,
    scene: &Scene,
    size: (u32, u32),
    panel: &D2D_RECT_F,
) -> (u32, u32) {
    let (bitmap, mut target) = offscreen_target(renderer, wic, size);
    renderer.render(&mut target, scene, DPI);
    let (w, h) = size;
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    unsafe {
        target.rt.EndDraw(None, None).unwrap();
        bitmap
            .CopyPixels(std::ptr::null(), w * 4, &mut pixels)
            .unwrap();
    }
    let lit = |x: u32| {
        (panel.top as u32 + 4..panel.bottom as u32 - 4).any(|y| {
            let p = &pixels[((y * w + x) * 4) as usize..][..4];
            p[..3].iter().any(|&c| c > 60)
        })
    };
    let columns = panel.left as u32..panel.right as u32;
    let first = columns.clone().find(|&x| lit(x)).unwrap();
    let last = columns.clone().rev().find(|&x| lit(x)).unwrap();
    (first - panel.left as u32, panel.right as u32 - 1 - last)
}

#[test]
#[ignore = "writes design screenshots to NB_SHOTS"]
fn render_island_states() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let wic: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.unwrap();
    let renderer = Renderer::new().unwrap();
    let palette = palette();
    let clock = ClockText {
        time: wide("6:47"),
        date: wide("27/11/2026"),
    };
    let pill = |width: f32| D2D_RECT_F {
        left: 16.0,
        top: 12.0,
        right: 16.0 + width,
        bottom: 56.0,
    };
    let base = |island: IslandView| Scene {
        metrics: Metrics::default(),
        palette,
        backdrop: Background::Clear,
        start: (D2D_RECT_F::default(), Pointer::default()),
        buttons: Vec::new(),
        status_rect: D2D_RECT_F::default(),
        status_pointer: Pointer::default(),
        status: StatusView::default(),
        clock_rect: D2D_RECT_F::default(),
        clock_pointer: Pointer::default(),
        clock: ClockText {
            time: clock.time.clone(),
            date: clock.date.clone(),
        },
        divider: None,
        status_reveal: Reveal::SHOWN,
        start_reveal: Reveal::SHOWN,
        start_scale: 1.0,
        clock_reveal: Reveal::SHOWN,
        origin: (0.0, 0.0),
        island: Some(island),
    };
    let collapsed = |panel: D2D_RECT_F, asleep: bool, scale: f32, music: bool| {
        let l = island::rest_layout(&panel);
        IslandView {
            panel,
            radius: (panel.bottom - panel.top) / 2.0,
            fill: color(0.027, 0.027, 0.031, 1.0),
            border: D2D1_COLOR_F::default(),
            content: false,
            clip: false,
            squircle: true,
            shadow: 1.0,
            badge: None,
            rest: Some(RestView {
                opacity: 1.0,
                music_mix: if music { 1.0 } else { 0.0 },
                apps: Vec::new(),
                app_rect: l.app,
                mascot: Some(MascotView {
                    opacity: 1.0,
                    rect: l.mascot,
                    asleep,
                    scale,
                    z: asleep.then_some(0.5),
                }),
                clock: Some((clock.time.clone(), l.clock)),
                music: music.then(|| MusicView {
                    art: Some(cover(&wic, 30)),
                    art_rect: l.art,
                    title: wide("Đêm thành phố"),
                    title_rect: l.title,
                    artist: wide("Lo-fi Radio"),
                    artist_rect: l.artist,
                    bars: [0.75, 1.0, 0.55, 0.3],
                    bars_rect: l.bars,
                }),
                text: palette.text,
                secondary: color(1.0, 1.0, 1.0, 0.6),
                accent: palette.accent,
            }),
        }
    };
    shot(
        &renderer,
        &wic,
        &base(collapsed(pill(200.0), false, 1.0, false)),
        (232, 70),
        "rest",
    );
    shot(
        &renderer,
        &wic,
        &base(collapsed(pill(200.0), true, 1.0, false)),
        (232, 70),
        "asleep",
    );
    shot(
        &renderer,
        &wic,
        &base(collapsed(pill(288.0), false, 1.0, true)),
        (320, 70),
        "music",
    );

    // At rest with an app in front: its small icon in the mascot's place.
    {
        let loader = crate::icons::IconLoader::new().unwrap();
        let app = crate::tasks::AppId {
            aumid: Some("Chrome".to_string()),
            exe: None,
        };
        let mut view = collapsed(pill(200.0), false, 1.0, false);
        if let (Some(rest), Some(icon)) = (&mut view.rest, loader.load_app(&app, None, 24)) {
            rest.mascot = None;
            rest.apps = vec![(icon, 1.0)];
        }
        shot(&renderer, &wic, &base(view), (232, 70), "rest-app");
    }

    // The expanded strip with five apps.
    let config = BarConfig::default();
    let height = island::strip_height(&config, 24.0);
    let percent = wide("100%");
    let (percent_w, clock_w) = renderer.strip_text(Some(&percent), &clock);
    let tail = island::Tail {
        glyphs: 3,
        percent: percent_w,
        clock: clock_w,
    };
    let layout = island::strip_layout(560.0, height, &config, 24.0, 20.0, 5, tail, false);
    let states = [
        ButtonState::Pinned,
        ButtonState::Running,
        ButtonState::Active,
        ButtonState::Minimized,
        ButtonState::Running,
    ];
    let mut view = collapsed(layout.panel, false, 1.0, false);
    view.rest = None;
    view.content = true;
    let mut scene = base(view);
    scene.buttons = layout
        .buttons
        .iter()
        .zip(states)
        .map(|(rect, state)| ButtonView {
            rect: *rect,
            icon: None,
            state,
            pointer: Pointer {
                hover: false,
                pressed: false,
            },
            scale: 1.0,
            lift: 0.0,
            reveal: Reveal::SHOWN,
        })
        .collect();
    // Real icons, loaded the way the bar loads them.
    let loader = crate::icons::IconLoader::new().unwrap();
    let apps: Vec<Option<Icon>> = [
        "Microsoft.Windows.Explorer",
        "Claude_pzs8sxrjxfjjc!Claude",
        "Chrome",
        "Microsoft.WindowsTerminal_8wekyb3d8bbwe!App",
        "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App",
    ]
    .iter()
    .map(|aumid| {
        let app = crate::tasks::AppId {
            aumid: Some((*aumid).to_string()),
            exe: None,
        };
        loader.load_app(&app, None, 24)
    })
    .collect();
    for (button, icon) in scene.buttons.iter_mut().zip(&apps) {
        button.icon = icon.as_ref();
    }
    scene.start = (layout.start, Pointer::default());
    scene.status_rect = layout.status;
    scene.status = StatusView {
        icons: [0xE701u16, 0xE995, 0xEBAA]
            .into_iter()
            .map(|glyph| StatusIcon { glyph, track: None })
            .collect(),
        percent: Some(wide("100%")),
    };
    scene.clock_rect = layout.clock;
    scene.divider = Some(layout.divider);
    for button in &mut scene.buttons {
        button.pointer = Pointer::default();
    }
    let size = (560, height as u32);
    shot(&renderer, &wic, &scene, size, "strip");
    // The two end margins, measured on the pixels of the drawn strip.
    let (left, right) = end_margins(&renderer, &wic, &scene, size, &layout.panel);
    println!("margins: left {left} px, right {right} px");
    assert!((left as i32 - right as i32).abs() <= 1, "{left} vs {right}");
    // Hover plates, for the eye.
    scene.start.1.hover = true;
    scene.buttons[1].pointer.hover = true;
    scene.buttons[1].scale = 1.2;
    shot(&renderer, &wic, &scene, size, "strip-hover");
    scene.start.1.hover = false;
    scene.buttons[1].pointer.hover = false;
    scene.buttons[1].scale = 1.0;

    // Frames of the opening and the closing movement, with the time it takes to
    // draw each one.
    let pill_rect = island::geometry(560.0, height, &config, false).pill;
    let middle = |r: &D2D_RECT_F| (r.left + r.right) / 2.0;
    let mut centers: Vec<f32> = vec![middle(&layout.start)];
    centers.extend(layout.buttons.iter().map(middle));
    centers.push(middle(&layout.status));
    centers.push(middle(&layout.clock));
    let start = std::time::Instant::now();
    let mut anim = island::Island::collapsed();
    let mut times = Vec::new();
    for (opening, name, begin, length) in [(true, "open", 0u64, 520u64), (false, "close", 600, 380)]
    {
        let from = start + std::time::Duration::from_millis(begin);
        anim.set_target(opening, from);
        for ms in (0..=length).step_by(20) {
            anim.tick(from + std::time::Duration::from_millis(ms));
            let t = anim.eased();
            let panel = island::morph_panel(&pill_rect, &layout.panel, t, anim.squash());
            let reveals = island::reveals(&centers, middle(&layout.panel), 42.0, &anim);
            let mut view = collapsed(pill_rect, false, 1.0, false);
            view.panel = panel;
            view.radius = (panel.bottom - panel.top) / 2.0;
            if let Some(rest) = &mut view.rest {
                rest.opacity = island::rest_opacity(&anim);
            }
            view.content = !anim.is_collapsed();
            view.clip = anim.animating();
            scene.island = Some(view);
            scene.start_reveal = reveals[0];
            for (button, reveal) in scene.buttons.iter_mut().zip(&reveals[1..]) {
                button.reveal = *reveal;
                button.pointer.pressed = false;
            }
            scene.status_pointer.hover = false;
            scene.status_reveal = reveals[reveals.len() - 2];
            scene.clock_reveal = reveals[reveals.len() - 1];
            let drawn = std::time::Instant::now();
            shot(
                &renderer,
                &wic,
                &scene,
                (560, height as u32),
                &format!("{name}-{ms:03}"),
            );
            times.push(drawn.elapsed());
        }
    }
    // Includes creating the target and writing the file: an upper bound.
    let timed = std::time::Instant::now();
    let rounds = 60;
    measure(&renderer, &wic, &scene, (560, height as u32), rounds);
    println!(
        "draw: {:.2} ms per frame",
        timed.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds)
    );
}
