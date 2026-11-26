//! Design screenshots without touching the screen: draws the island's states with
//! the real renderer into a bitmap and writes magnified BMP files.
//!
//! `NB_SHOTS=<folder> cargo test --release render_island_states -- --ignored`

use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory,
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
        let mut target = Target {
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
            content_opacity: 0.0,
            squircle: true,
            shadow: 1.0,
            badge: None,
            rest: Some(RestView {
                opacity: 1.0,
                music_mix: if music { 1.0 } else { 0.0 },
                mascot: Some(MascotView {
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

    // The expanded strip with five apps.
    let config = BarConfig::default();
    let height = island::strip_height(&config, 24.0);
    let layout = island::strip_layout(560.0, height, &config, 24.0, 5, 122.0, false);
    let states = [
        ButtonState::Pinned,
        ButtonState::Running,
        ButtonState::Active,
        ButtonState::Minimized,
        ButtonState::Running,
    ];
    let mut view = collapsed(layout.panel, false, 1.0, false);
    view.rest = None;
    view.content_opacity = 1.0;
    let mut scene = base(view);
    scene.buttons = layout
        .buttons
        .iter()
        .zip(states)
        .enumerate()
        .map(|(i, (rect, state))| ButtonView {
            rect: *rect,
            icon: None,
            state,
            pointer: Pointer {
                hover: false,
                pressed: i == 1,
            },
            scale: 1.0,
            lift: 0.0,
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
    scene.status_rect = layout.status;
    scene.status_pointer.hover = true;
    scene.status = StatusView {
        icons: [0xE701u16, 0xE995, 0xEBAA]
            .into_iter()
            .map(|glyph| StatusIcon { glyph, track: None })
            .collect(),
        percent: Some(wide("100%")),
    };
    scene.clock_rect = layout.clock;
    scene.divider = layout.divider;
    shot(&renderer, &wic, &scene, (560, height as u32), "strip");
}
