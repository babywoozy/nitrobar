# nitrobar — quy ước dự án

Taskbar thay thế cho Windows 11, viết bằng Rust. Ưu tiên số 1: **nhẹ và nhanh**.

## Lệnh thường dùng

```powershell
cargo build --release                               # build chính thức
cargo clippy --release --all-targets -- -D warnings # phải sạch trước khi commit
cargo fmt                                           # format trước khi commit
cargo test --release                                # unit test (config, layout)
.\target\release\nitrobar.exe [--hide-taskbar]      # chạy
.\target\release\nitrobar.exe --quit                # tắt instance đang chạy
.\target\release\nitrobar.exe --restore-taskbar     # cứu taskbar gốc nếu bị kẹt ẩn
```

Bản debug có console (thấy log `debug_log!`); bản release là GUI subsystem, không có console.

## Ngân sách hiệu năng (bắt buộc)

- RAM: private bytes < 20 MB (bắt buộc), mục tiêu < 10 MB. Working set tính cả trang DLL hệ thống dùng chung nên chỉ để tham khảo. Đo được: GĐ1 4.6 MB / 17 MB; GĐ2 9.1 MB / 31 MB (WIC + shell COM cho icon); GĐ3 9.3 MB / 31 MB; GĐ4 9.5 MB / 32 MB; GĐ5 9.2 MB / 33 MB, exe 486 KB; ghim từ Windows 12.1 MB / 48 MB (property system của shell); icon nét 12.0 MB; nền clear 11.5 MB (sau khi đổi qua lại 4 chế độ: 14.5 MB); chống thanh đen 12.6 MB; island 8.6 MB thu gọn / ~14.5 MB sau khi bung, exe 589 KB; hiệu ứng thu nhỏ 9.0 MB (sau thu nhỏ + khôi phục), ~14.7 MB sau nhiều lần bung, exe 603 KB; island mới (dải icon + bấm icon + xem trước) 12.5 MB sau thu nhỏ / khôi phục / xem trước, exe 627 KB.
- CPU: 0 ms khi không có sự kiện. Đo lại sau mỗi giai đoạn (Process.TotalProcessorTime trong 5 giây).
- Không polling. Mọi cập nhật đến từ sự kiện: WinEvent hook, shell hook, appbar callback,
  WM_SETTINGCHANGE, notification COM... Timer chỉ dùng khi không có sự kiện tương ứng, và
  phải là one-shot căn theo thời điểm cần (ví dụ đồng hồ: đúng biên phút), không phải tick đều.
- Direct2D dùng **software render target** (`D2D1_RENDER_TARGET_TYPE_SOFTWARE`). Bản GPU
  kéo theo D3D11 + driver, tốn thêm ~30 MB private. Chỉ đổi nếu có số đo chứng minh.

## Phụ thuộc

- Crate đang dùng: `windows` (Win32), `windows-core` (cho `#[implement]`), `windows-numerics`
  (`Matrix3x2` cho `SetTransform` / layer của Direct2D; cả hai đã có sẵn trong cây phụ thuộc của
  `windows`, không nặng thêm), `serde` (derive) + `toml` (tắt default features) cho config.toml /
  pins.toml. Không dùng framework GUI (egui, Tauri, webview, winit...).
- Thêm crate mới phải có lý do rõ ràng và ghi vào đây.
- Feature của `windows` bật tối thiểu; thêm feature khi cần, giữ danh sách theo thứ tự ABC.

## Kiến trúc

```
src/
  main.rs     parse args, DPI awareness, cài safety, chạy app
  app.rs      cửa sổ thanh, message loop, window procedure, struct App
  appbar.rs   bọc SHAppBarMessage (đăng ký, đặt vị trí, thông báo)
  taskbar.rs  ẩn / hiện taskbar gốc, file trạng thái để phục hồi sau crash
  safety.rs   panic hook, console ctrl handler, SEH filter -> emergency_cleanup()
  render.rs   tài nguyên Direct2D / DirectWrite, vẽ toàn bộ thanh
  clock.rs    chuỗi giờ / ngày theo locale người dùng
  layout.rs   hình học (DIP) dùng chung cho vẽ và hit-test
  tasks.rs    danh sách cửa sổ, gom nhóm theo AppId, WinEvent hook, click/minimize
  icons.rs    icon thật -> IWICBitmap PBGRA đúng cỡ pixel (AUMID, relaunch icon, exe, WM_GETICON)
  menu.rs     menu chuột phải (TrackPopupMenu, chạy NGOÀI borrow của App)
  theme.rs    sáng/tối + màu accent từ registry, Palette, nền clear/blur/acrylic/solid, menu tối
  shell.rs    mở UI của Windows bằng phím tắt giả lập (Start = Win, Quick Settings = Win+A,
              thông báo = Win+N)
  status/     wifi / âm lượng / pin: mod.rs (gộp thông báo, StatusView), network.rs,
              volume.rs (COM callback), battery.rs
  config.rs   %APPDATA%\nitrobar\config.toml: parse + clamp, file mặc định có chú thích, màu
  pins.rs     danh sách ghim = ghim của Windows + pins.toml ([[pin]] thêm, [[hide]] bỏ), mở app
  winpins.rs  đọc ghim của taskbar gốc (blob Taskband\Favorites -> PIDL), theo dõi registry
  island.rs   chế độ island: hiệu ứng (lò xo), hình học viên + "activity" (Pulse), cảm biến mép,
              flyout, fullscreen
  morph.rs    hiệu ứng thu nhỏ / khôi phục: overlay + DWM thumbnail, đồng hồ khung hình (DwmFlush)
  preview.rs  xem trước các cửa sổ của một app (popup GDI + DWM thumbnail)
  tray.rs     nhận icon khay (cửa sổ Shell_TrayWnd đứng trước Explorer), bấm icon
  tray_panel.rs  bảng icon khay (cửa sổ layered); render/tray.rs vẽ nó
  glass.rs    blur phía sau viên (Windows.UI.Composition)
  wallpaper.rs  màu chủ đạo của hình nền (luồng nền)
  media.rs    bài đang phát (SMTC, luồng riêng), ảnh bìa tròn, độ to âm thanh, cột sóng
  mascot.rs   linh vật Nitro: trạng thái ngủ / thức
  render/offscreen.rs  chụp thiết kế ra file, không đụng màn hình
```

### Cấu hình và ghim (config.rs, pins.rs)

- `DEFAULT_CONFIG` (có chú thích) phải parse ra đúng `Config::default()`: có unit test. Thêm
  khoá mới thì sửa cả hai, cả README.
- Tải lại trực tiếp: `FindFirstChangeNotificationW` trên thư mục config, handle được chờ ngay
  trong message loop (`MsgWaitForMultipleObjects`), không có thread riêng. File lỗi -> giữ
  config cũ, lỗi hiện thành mục mờ trong menu chuột phải.
- Kích thước lấy từ `layout::Metrics` (theo config), không dùng hằng số cố định cho thanh/nút/icon.
- Pins tách khỏi config.toml để app ghi lại mà không xoá chú thích của người dùng.
- Ghim của Windows: `HKCU\...\Explorer\Taskband\Favorites` = byte 0, rồi mỗi ghim `u32` size +
  PIDL tuyệt đối + 1 byte (0 = còn tiếp, 0xFF = hết). App desktop trỏ tới .lnk trong
  `Quick Launch\User Pinned\TaskBar`; app Store trỏ thẳng vào `shell:AppsFolder` (không có .lnk).
  Định dạng không công khai: kiểm tra độ dài từng item, lỗi thì quay về đọc các .lnk trong thư mục.
  Không bao giờ ghi vào danh sách của Windows. Key thay đổi -> `RegNotifyChangeKeyValue` (event
  chờ trong message loop) -> `WM_APP_PINS`.
- Ghim khớp với app đang chạy bằng `AppId::same_app`: cùng AUMID nếu cả hai có, nếu không thì cùng
  exe. AUMID của cửa sổ: property store -> AUMID của package (`GetApplicationUserModelId`; app
  MSIX/Electron từ Store không đặt AUMID cho cửa sổ) -> `Microsoft.Windows.Explorer` cho explorer.exe.
- Thứ tự nút: app ghim (mỗi ghim gộp với một nhóm đang chạy khớp) rồi app đang chạy chưa ghim.
  `Hit::Task(i)` là chỉ số trong danh sách gộp này (`App::items()`). Trong lúc menu mở, danh sách
  có thể đổi: lệnh tìm lại app theo `AppId`, không theo chỉ số.
- toml: mảng rỗng phải `skip_serializing_if` (ghi `x = []` sau `[[bảng]]` làm serializer lỗi).

### Trạng thái hệ thống (status/)

- **Không bao giờ gọi dịch vụ hệ thống có thể chậm trên UI thread.** WLAN service có lúc mất
  vài giây (lần đầu sau khi boot): GĐ4 từng treo UI thread ở `LpcReply` vì vậy. Mạng được khởi
  tạo trên thread `nitrobar-network`, trạng thái tính lại ngay trong callback (thread pool) và
  lưu vào `Mutex`; UI chỉ đọc `network::current()`.
- Âm lượng: `IAudioEndpointVolumeCallback::OnNotify` mang sẵn level + mute -> lưu atomics;
  `IMMNotificationClient::OnDefaultDeviceChanged` -> UI thread re-bind thiết bị mới.
- Pin: `RegisterPowerSettingNotification` (phần trăm pin, nguồn AC) -> `WM_POWERBROADCAST`.
  Máy không có pin thì ẩn icon pin.
- Callback ở thread khác chỉ gọi `status::notify(bit)`: gộp bit vào `PENDING`, post một
  `WM_APP_STATUS`. UI chỉ vẽ lại khi `StatusView` thay đổi.
- Icon dùng font "Segoe Fluent Icons" (dự phòng "Segoe MDL2 Assets"). Mã đã kiểm tra bằng
  cách render: wifi E872/E873/E874/E701, offline F384, ethernet E839, loa E992-E995, mute
  E74F, pin EBA0+n, đang sạc EBAB+n (n = 0..10).
- `#[implement]` cần `windows-core` là dependency trực tiếp (đã có sẵn qua `windows`).

### Giao diện (theme.rs, render.rs)

- Màu lấy từ `Palette` (theme.rs), không hard-code màu trong render.rs. Theme đọc
  `SystemUsesLightTheme` và `Explorer\Accent\AccentPalette` (tối dùng Light2, sáng dùng Dark1,
  giống chỉ báo của taskbar Win11). Nạp lại khi có WM_SETTINGCHANGE / WM_THEMECHANGED /
  WM_DWMCOLORIZATIONCOLORCHANGED.
- Nền (`[bar] background`): mọi chế độ trong suốt đều `DwmExtendFrameIntoClientArea(-1)` + vẽ
  pixel trong suốt. `clear` = không accent (thấy thẳng phía sau); `blur` = accent acrylic với tint
  gần 0 + tint nhẹ tự vẽ; `acrylic` = accent acrylic, DWM tự tint (`GradientColor` ABGR); `solid` =
  margins 0, vẽ đặc. Accent qua `SetWindowCompositionAttribute` (user32, không công khai, như
  TranslucentTB). Acrylic/blur của accent lấy mẫu hình nền, không lấy cửa sổ phía sau.
- KHÔNG dùng `DWMWA_SYSTEMBACKDROP_TYPE` (Mica/Acrylic hệ thống): DWM chỉ vẽ nó cho cửa sổ
  "active"; mẹo trả `WM_NCACTIVATE(TRUE)` hỏng khi panel của Windows (thông báo, Quick Settings)
  lấy foreground rồi đóng => thanh đen đặc không tự hết. Luôn đặt nó về `DWMSBT_NONE`.
- `ACCENT_ENABLE_BLURBEHIND` (3) ra màu đen với cửa sổ thường trên Win11 hiện tại: không dùng.
- `clear` dùng `ACCENT_ENABLE_TRANSPARENTGRADIENT` (2) với màu alpha 0, KHÔNG `ACCENT_DISABLED`:
  về DISABLED sau khi đã bật accent acrylic thì pixel trong suốt thành đen (đã tái hiện:
  acrylic -> clear). Chỉ `solid` dùng DISABLED (không có pixel trong suốt).
- Tự phục hồi (`App::recover`: bỏ render target, áp lại backdrop, đặt lại vị trí, vẽ lại) khi:
  mở khoá máy (`WM_WTSSESSION_CHANGE`/`WTS_SESSION_UNLOCK`, cần `WTSRegisterSessionNotification`),
  thức dậy sau sleep (`PBT_APMRESUMEAUTOMATIC`/`RESUMESUSPEND`), `WM_DWMCOMPOSITIONCHANGED`,
  Explorer khởi động lại. `WM_THEMECHANGED`/`WM_DWMCOLORIZATIONCOLORCHANGED` luôn áp lại backdrop.
  `D2DERR_RECREATE_TARGET` -> vẽ lại ngay (trước đây cửa sổ giữ nguyên hình cũ / đen).
- Kiểm thử chống "thanh đen": mở + đóng trung tâm thông báo, Quick Settings, Start, Alt+Tab ở
  cả clear và acrylic, đổi acrylic -> clear, đổi theme sáng/tối (registry + broadcast
  `ImmersiveColorSet`, nhớ trả lại). Khoá máy / sleep không tự test được: post message giả.
- Khi trong suốt: text antialias GRAYSCALE (ClearType cần nền đặc). Với clear/blur, chữ, glyph
  trạng thái và vạch chỉ báo có bóng (vòng 1 DIP alpha 0.4 + bóng lệch xuống alpha 0.5, màu
  tương phản với chữ) để đọc được trên nền trắng.
- Hover dùng `TrackMouseEvent(TME_LEAVE)`; nhấn giữ thu nhỏ icon (24 -> 20 DIP) như Win11.
- Menu tối dùng uxtheme ordinal 135/136 (không công khai); nếu mất thì menu chỉ sáng lại.

### Danh sách app (tasks.rs)

- Lọc cửa sổ theo luật của Explorer: visible, không WS_EX_TOOLWINDOW/NOACTIVATE và không có
  owner (trừ khi WS_EX_APPWINDOW), không bị cloak (desktop ảo khác), có tiêu đề.
- Gom nhóm theo AppUserModelID nếu có, nếu không thì theo đường dẫn exe (giống Windows 11:
  Chrome nhiều profile = nhiều nhóm). Thứ tự nhóm ổn định: nhóm mới thêm vào cuối.
- Hook chỉ đăng ký các khoảng event hẹp (FOREGROUND, DESTROY..HIDE, NAMECHANGE, CLOAKED..UNCLOAKED).
  KHÔNG hook LOCATIONCHANGE / khoảng rộng: chúng bắn liên tục và đốt CPU.
- Callback hook không đụng `App`; chỉ gọi `request_refresh()` (gộp nhiều event thành một
  `WM_APP_REFRESH`). Refresh = EnumWindows + diff, chỉ vẽ lại khi chữ ký thay đổi.
- Bấm: app đang focus + 1 cửa sổ -> thu nhỏ; nhiều cửa sổ -> xoay vòng; khác -> kích hoạt
  cửa sổ dùng gần nhất. Dùng `ShowWindowAsync` cho cửa sổ của process khác (không treo nếu app treo).
- Icon (icons.rs), thứ tự: AUMID -> `shell:AppsFolder` (logo package) -> `RelaunchIconResource` của
  cửa sổ (Chrome: icon profile có huy hiệu) -> icon của exe đúng cỡ pixel (resource 24/32/48 vẽ
  tay, nét hơn thu nhỏ icon 32 của cửa sổ) -> icon cửa sổ (`WM_GETICON`, cho exe "host" như java,
  python, rundll32).
- HBITMAP của `IShellItemImageFactory::GetImage` thường là alpha THẲNG (không premultiplied), đôi khi
  premultiplied, icon cũ thì alpha toàn 0: `classify_alpha` xem pixel (màu > alpha => thẳng). Coi
  nhầm alpha thẳng là premultiplied => góc trong suốt sáng lên thành viền rỗ.
- Không phóng to icon nhỏ (`Icon::px` <= cỡ yêu cầu, vẽ đúng cỡ đó). Vẽ icon bằng
  `layout::pixel_square`: cạnh rơi đúng pixel thiết bị ở mọi DPI => bitmap chép 1:1, không nhoè.
  So độ nét: chụp màn hình phóng to 4x nearest-neighbor cạnh taskbar gốc.
- `WM_GETICON` gửi bằng `SendMessageTimeoutW(SMTO_ABORTIFHUNG | SMTO_BLOCK)`: không treo vì app
  treo, và không nhận message re-entrant trong lúc chờ.

### Quy tắc trong window procedure

- `App` nằm trong `RefCell` ở `GWLP_USERDATA`. Win32 gọi lại wndproc đồng bộ (SetWindowPos
  gửi WM_WINDOWPOSCHANGED...), nên message đến khi `App` đang bị mượn sẽ rơi về
  DefWindowProc. Message nào cần chạy cả khi đó thì xử lý ở nhánh "stateless" đầu `wndproc`
  (chỉ dùng HWND).
- Không gọi `DestroyWindow` từ trong `App::handle`; để WM_CLOSE rơi về DefWindowProc.
- Kích thước render target được đồng bộ lúc vẽ, không phụ thuộc WM_SIZE.
- Toạ độ vẽ là DIP; render target đặt DPI = DPI màn hình. Toạ độ cửa sổ là pixel, đổi bằng `scale()`.

### Chế độ island (island.rs, phần "island mode" trong app.rs)

- `[bar] mode = "island"` (mặc định) | `"full"`. Island = KHÔNG đặt vị trí AppBar (chỉ `ABM_NEW`,
  để vẫn nhận `ABN_FULLSCREENAPP`); đổi mode thì `ABM_REMOVE` + `ABM_NEW` để nhả chỗ đã giữ.
  Island ngầm ẩn taskbar gốc (`wants_native_hidden`), nếu không cửa sổ phóng to không tràn xuống đáy.
- Không gian vẽ = "bar space" (DIP của cả dải thanh). Khi thu gọn, cửa sổ = `Geometry::activity_hit`
  (đủ chỗ cho viên phồng khi nảy) nhưng có window region = `Geometry::hit` (viên + lề hover), nên
  không chặn click chỗ khác; lúc nảy thì bỏ giới hạn region. Đổi region KHÔNG dời hình đã vẽ; đổi
  kích thước / vị trí cửa sổ thì DWM hiện khung cũ ở vị trí mới trong 1 frame (viên nhảy lên trên,
  đã thấy) => tránh resize khi chỉ cần đổi vùng nhận chuột. `App::hit_rect` = vùng đó (cho
  `cursor_inside`). `App::origin` là độ lệch của cửa sổ trong bar space, renderer
  `SetTransform(-origin)`, `hit_at` cộng origin. Lúc bung / đang co cửa sổ là cả dải; về hẳn trạng
  thái thu gọn mới thu cửa sổ lại. Đổi kích thước xong gọi `paint()` ngay. KHÔNG `UpdateWindow` trong
  lúc đang mượn `App`. Về mode full thì `SetWindowRgn(None)`.
- Phát hiện chạm mép: `EdgeSensor` = cửa sổ layered alpha 1/255 cao 1 px dọc mép (alpha 0 thì không
  được hit-test), chỉ hiện khi thu gọn; post `WM_APP_EDGE`. Không quét chuột: chỉ `WM_MOUSEMOVE`,
  `TrackMouseEvent(TME_LEAVE)` và timer one-shot: `TIMER_EXPAND` (trễ bung), `TIMER_COLLAPSE` (trễ thu;
  nếu bị chặn thì hẹn lại, chỉ khi đang mở và bị chặn), `TIMER_PULSE` (hết lúc viên giữ yên).
- Viên lúc nghỉ: đen (`REST_FILL`), bo tròn hết, bóng đổ giả bằng 8 lớp `FillRoundedRectangle` alpha
  thấp chồng nhau (cửa sổ phải chừa `HIT_MARGIN` 14 DIP để bóng không bị cắt; effect của D2D cần GPU
  nên không dùng). Bên trái icon của app active (`App::rest_icon`, nhóm chứa `tasks.foreground`;
  cửa sổ không có nút như desktop / flyout giữ icon cũ), đổi thì crossfade 240 ms; bên phải giờ.
  Con số trong config nhớ rằng file config.toml ĐÃ TẠO TRƯỚC KHI đổi mặc định vẫn ghi giá trị cũ
  (`island_width = 120`...): khi test thiết kế mới phải tạm đổi config (`cfgswap.ps1`) và trả lại.
  `island::strip_height` = dải cao tới mức chứa viên nghỉ / activity / dải icon + gap + lề (cửa sổ
  island KHÔNG liên quan `bar_height`, dùng `icon_size`).
- Dải icon (`island::strip_layout`, thuần hình học, có test): các nút ở VỊ TRÍ CUỐI, panel lớn dần từ
  viên tới `StripLayout::panel` (nội dung mờ vào qua layer, kẹp vào panel). Panel = viên bo tròn hết,
  nền đen cố định; nút thừa thì bỏ. Cửa sổ khi mở = `StripLayout::window` (panel + bóng + chỗ cho độ
  vượt của lò xo ~3%) hợp với `activity_hit`; danh sách app đổi lúc đang mở thì `place_island` lại.
  Bung ra theo lò xo (`EXPAND_SPRING`), thu lại ease-in-out. Không có nút Start (Win mở Start).
  Màu nội dung = `App::island_palette` (trắng trên đen bất kể theme); `text_shadow` tắt trong island.
- Kính (`island_style = "glass"`, `glass.rs`): blur THẬT phía sau viên (cả cửa sổ app) bằng
  Windows.UI.Composition `CreateHostBackdropBrush` (+ `DWMWA_USE_HOSTBACKDROPBRUSH`), cắt bằng
  `CompositionRoundedRectangleGeometry` (khử răng cưa, góc bo theo đúng viên). Đã đo: chạy được cả với
  cửa sổ KHÔNG BAO GIỜ active (khác DWMSBT / accent chỉ lấy hình nền). NHƯNG output của Composition vẽ
  ĐÈ LÊN bề mặt GDI / D2D của cùng cửa sổ (icon, chữ biến mất, ngoài visual là nền đen) => kính nằm
  trong cửa sổ RIÊNG `NitrobarGlass` ngay dưới cửa sổ thanh (`WS_EX_NOREDIRECTIONBITMAP` = trong suốt
  ngoài visual, click xuyên qua), `Glass::place` đặt cùng rect + ngay dưới cửa sổ thanh, `set_shape`
  mỗi lần vẽ theo panel đang chạy. Cửa sổ thanh vẽ lớp màu (tint) trong suốt + viền sáng 1 px + icon
  + chữ; bóng đổ TẮT (nó sẽ làm tối lớp blur nằm dưới). Cần `DispatcherQueue` trên thread UI
  (`CreateDispatcherQueueController`, `DQTAT_COM_NONE`). Không có Composition => `glass` là `None`,
  viên vẫn vẽ tint đậm hơn (không blur). RAM không đổi đáng kể (~11.5 MB), exe +19 KB.
  (BỎ lấy mẫu nền: giờ màu cố định, xem `island_base` / `dark_ink`; mô tả cũ bên dưới chỉ để tham khảo.)
  Màu chữ / icon (cũ): `App::refresh_backdrop` lấy mẫu ~9 pixel màn hình sát ngoài viên (GetPixel; KHÔNG
  polling: gọi khi bung ra / đổi foreground / đổi style), độ sáng > 0.62 => nền sáng (tint trắng, chữ
  tối `island_palette`), < 0.48 => tối (có độ trễ chống chớp).
- Zoom: CHỈ icon đang hover (`App::hover == Task(i)`) phóng 1.2x (`ZOOM_MAX`), `App::zoom[i]` (0..1)
  của từng nút đuổi theo đích bằng ease 50 ms trong `on_frame` (frame chỉ chạy khi còn đang đổi),
  nút vừa rời thì thu lại riêng. (Bản đầu zoom theo khoảng cách bằng Gaussian: hai icon cùng to khi
  chuột ở giữa, bị bỏ.)
- Vùng kích hoạt: `EdgeSensor` chỉ rộng `viên + 2 * EDGE_ZONE_MARGIN` (40 DIP) ở giữa mép; trước đây
  cả chiều ngang màn hình nên rê chuột xuống đáy cửa sổ app khác (ô chat) cũng bung dải.
  `expand_delay_ms` mặc định 30 (0 = bung ngay), bung 220 ms lò xo (40 rad/s, 0.72).
- `config::migrate_legacy`: file config tạo bởi bản island đầu ghi sẵn 120x8 / 100 ms; giá trị trong
  file luôn thắng mặc định nên người dùng không bao giờ thấy thiết kế mới. Các dòng còn nguyên văn
  như bản cũ ghi (chưa bị sửa) được tự nâng lên mặc định mới; dòng đã sửa thì giữ. Icon phóng to vẽ từ bitmap cỡ gốc nên hơi mềm; ở 1x vẫn 1:1 theo
  pixel. Hit-test dùng rect không phóng to. `ButtonState::Minimized` = mọi cửa sổ iconic => chấm mờ.
- Khung hình: `morph::start_clock` = thread `nitrobar-frames` gọi `DwmFlush` (chờ lần compose kế
  tiếp = theo tần số màn hình, 60/120/144 Hz) rồi post `WM_APP_FRAME` (tối đa 1 message đang chờ).
  Dùng chung cho bung/thu, viên nảy, overlay thu nhỏ; `on_frame` dừng thread khi mọi thứ đứng yên.
  `DwmFlush` có thể trả về ngay khi DWM không có gì để vẽ => ngủ 4 ms nếu < 3 ms (không quay CPU).
- Bung ra không do chuột (phím Win) thì gọi `track_mouse()` ngay: con trỏ không ở trên thanh =>
  `WM_MOUSELEAVE` lập tức => hẹn thu lại. Hết giờ thu mà con trỏ đã quay lại (chưa có
  `WM_MOUSEMOVE`) thì chỉ track lại, không thu.
- Không thu khi: menu chuột phải (`menu_open`, đặt quanh `menu::show`), flyout của Windows ở
  foreground (`is_shell_flyout`: SearchHost = Start, ShellHost = Quick Settings,
  ShellExperienceHost = thông báo, StartMenuExperienceHost trên bản cũ), đang nhấn chuột trái
  (kéo thả). Phím Win = Start lên foreground => bung ra (dùng hook foreground sẵn có, không hook bàn phím).
- Toàn màn hình: `ABN_FULLSCREENAPP` + kiểm tra foreground (phủ kín màn hình, không `IsZoomed`, không
  caption; cửa sổ phóng to có rect lố ra -8 px nên phải loại bằng `IsZoomed`) => ẩn viên + sensor,
  chạm mép không bung.
- Hiệu ứng: `Island` (vị trí tuyến tính theo thời gian, vẽ bằng `ease_out_cubic`), đảo chiều giữa
  chừng thì chỉ tốn phần còn lại. Khung morph từ viên -> cả thanh; icon hiện dần qua layer
  (opacity, clip vào khung). Dùng lại MỘT `ID2D1Layer` cho mỗi render target: `PushLayer(.., None)`
  cấp layer mới mỗi frame, RAM riêng lên 16-17 MB thay vì 14 MB.
- Cửa sổ island luôn dùng accent "clear"; khung do mình vẽ (đen, viền mảnh); app cần chú ý => màu
  `attention`. `[bar] background` / `[colors]` chỉ dùng cho mode full.
- Đo: thu gọn 8.6 MB, sau nhiều lần bung/thu ~14.5 MB; CPU 0 khi đứng yên; ~57 ms CPU cho mỗi lần
  chuyển động ~180 ms (software render cả dải 60 fps).
- "Activity" (`Pulse`): app vào / ra island => viên phồng tới `Geometry::activity` (+48 DIP rộng, cao
  34 DIP, giữ mép dưới) bằng lò xo (`morph::spring`, vượt ~20%), đổi màu đen (`PULSE_FILL`), hiện icon
  app (`render::Badge`, vẽ đúng cỡ pixel), giữ `PULSE_HOLD` 1.5 s (không vẽ frame nào, chỉ
  `TIMER_PULSE`), rồi lò xo về. Chỉ khi đang là viên (bung ra thì không).

- Bấm icon (`App::click_island`, mode full vẫn dùng nhánh cũ trong `activate`): 1 cửa sổ + đang thu nhỏ
  => `start_restore` (overlay bung từ rect nút), `restore_collapse` => dải thu lại khi cửa sổ hiện;
  đang active => `tasks.click` (thu nhỏ), `GETMINRECT` trả rect NÚT (min_rect khi dải đang mở),
  `min_collapse` => thu dải + viên nảy lúc "nuốt"; ở sau => `tasks.click` + `start_bounce` (icon nhảy
  2 lần, `island::bounce_height`, vẽ qua `ButtonView::lift`); nhiều cửa sổ => `preview.rs`.
  Sau khi bấm dải thu lại và `suppress_expand` chặn bung lại tới khi con trỏ rời vùng một lần (con trỏ
  vẫn đứng ở chỗ icon cũ). Nếu overlay không chạy được (tắt hiệu ứng / đăng ký thumbnail lỗi) thì
  thu dải ngay (`WM_APP_MINIMIZED` xử lý cả hai trường hợp).
- `GETMINRECT` có thể tới lúc `App` đang bị mượn (ngay trong handler của click): nhánh stateless vẫn
  trả lời bằng `MIN_RECT` VÀ post `WM_APP_MINIMIZED` (wParam cửa sổ, lParam 1 = thu nhỏ / 0 = khôi
  phục); `App` làm phần còn lại. Trước đây việc đó nằm trong `App` nên 1/6 lần bị mất (không overlay,
  dải không thu).
- BÀI HỌC (lỗi "thu nhỏ app xong không mở lại được", đã tái hiện với Claude, VS Code):
  1. Overlay của hiệu ứng phủ KÍN màn hình (app đang phóng to) => Explorer coi nó là app toàn màn hình
     => `ABN_FULLSCREENAPP` => thanh tự ẩn + `morph.finish()` huỷ chính hiệu ứng đang chạy => cửa sổ
     không bao giờ được khôi phục (`tasks::activate` nằm cuối hiệu ứng). `morph::avoid_fullscreen` chừa
     mép 2 px. Mọi cửa sổ topmost của mình phủ kín màn hình đều dính cái này.
  2. `ShowWindowAsync` bị VS Code (Chromium) lờ đi hoàn toàn (cả thu nhỏ lẫn khôi phục; 8 s không đổi
     gì), `ShowWindow` đồng bộ thì chạy. `tasks::show` dùng `ShowWindow`, trừ app treo
     (`IsHungAppWindow`) dùng bản async. `GETMINRECT` gửi tới trong lúc ta đang gọi `ShowWindow`
     (App bị mượn) => nhánh stateless lo (đúng chỗ đã sửa trước đó).
  3. Tin nhắn post tới lúc `App` đang bị mượn (COM của shell bơm message) bị `DefWindowProc` nuốt:
     `on_busy` post lại (riêng `WM_APP_FRAME` thì chỉ bỏ cờ pending, nếu không đồng hồ khung hình
     đứng vĩnh viễn).
- Khôi phục: overlay có `BACKDROP` (2,2,2) làm colorkey => không còn khối đen, chỉ ảnh cửa sổ phóng
  to từ icon + mờ dần vào; thu nhỏ vẫn là khối đen (không dùng colorkey).
- Test app thật: `real2.ps1` (bản debug) thu nhỏ rồi bấm icon trên dải, bấm icon app active, mở lại,
  Alt+Tab, kiểm tra overlay không kẹt. Thu nhỏ bằng `ShowWindow` đồng bộ (PostMessage SC_MINIMIZE bị
  VS Code lờ đi). Không dùng `ShowWindowAsync` cho test vì lý do ở mục 2.
- Xem trước nhiều cửa sổ (`preview.rs`): popup không kích hoạt, vẽ bằng GDI (nền, ô, tiêu đề) + một DWM
  thumbnail mỗi cửa sổ (live, kể cả cửa sổ đang thu nhỏ), region bo tròn (mép hơi răng cưa). Báo cho
  thanh `WM_APP_PREVIEW`: ENTER / LEAVE (con trỏ vào / ra panel => `preview_hover` giữ dải mở) và PICK
  (cửa sổ được chọn: đang thu nhỏ thì tỏa ra từ icon, không thì kích hoạt, rồi thu dải). Đóng khi dải
  thu lại hoặc bấm lại icon. Tối đa 4 cột, quá thì xuống hàng.
- Test UI: bản debug in `BTN <khoá app> x y` (pixel màn hình) mỗi lần dải mở để script tìm nút, vì số
  nút phụ thuộc app người dùng đang mở. Tự bấm chuột thật chỉ khi con trỏ đang ở trên popup của mình
  (`mouse_event`), còn lại gửi `PostMessage` vào cửa sổ thanh.

### Thiết kế viên (quy tắc người dùng đặt, giữ nguyên khi sửa)

- Lề đều, mọi thứ căn giữa dọc; bo góc lồng nhau = bo góc viên − lề (viên tròn hết => phần tử tròn có
  tâm trùng đầu viên); chữ Segoe UI Variable: dòng chính trắng semibold, dòng phụ xám 60% nhỏ hơn;
  giờ semilight + số đều (`DWRITE_FONT_FEATURE_TAG_TABULAR_FIGURES`); MỘT màu nhấn
  (`App::island_accent`: ảnh bìa nếu đang phát, không thì `island_color`, đã kéo sáng), còn lại đen /
  trắng / xám (cả "cần chú ý" cũng dùng màu nhấn).
- Vị trí nội dung lúc thu gọn nằm hết ở `island::rest_layout` (dùng chung cho app và ảnh chụp, có
  test lề). `pill_padding`: nội dung = 0.68 * cao. Giờ cách đầu viên 0.31 * cao (nhìn cân với lề quanh
  linh vật; căn theo toán học 17 px thì trông lệch).
- Góc liên tục (`shape.rs`): superellipse (viên: số mũ 2.35, vươn 1.3 x nửa cao; plate hover: 3.4),
  dựng thành `ID2D1PathGeometry` đa giác. Kính (glass) vẫn bo tròn thường vì blur clip bằng rounded
  rect. KHÔNG viền ở kiểu đen (đường 1 DIP khử răng cưa theo cung trông đậm nhạt); bóng đổ = nét
  geometry rộng dần, alpha thấp. Không nội dung nào vào đoạn cong: cách hai đầu đúng nửa chiều cao.
- Giờ một dòng "6:47" (`[clock] hour_format` auto/12/24, không AM/PM), nét light; chữ số căn giữa
  theo baseline - cap height (`draw_time_centered`), không theo hộp dòng.
- Icon: xin shell ảnh LỚN rồi thu bằng WIC Fant (exe: max(48, 2px); app đóng gói: 4px) và giữ thêm
  bản 2x (`Icon::large`) cho lúc phóng to. Xin đúng 24 px thì File Explorer, Claude bị rỗ mép.
- Chụp thiết kế KHÔNG cần chạy app (không làm phiền người dùng):
  `NB_SHOTS=<thư mục> cargo test --release render_island_states -- --ignored` => BMP phóng 5x của
  nghỉ / ngủ / nhạc / dải. Đổi giao diện thì chụp và so trước khi chạy thật.
- Nitro: hai `ID2D1PathGeometry` trong hộp 24x24, vẽ bằng `SetTransform`. Ngủ: timer one-shot
  `TIMER_SLEEP` rồi hỏi `GetLastInputInfo` (không polling); "z" chạy `Z_STEPS` bước rồi đứng yên (CPU 0).
  Đang ngủ mà rê chuột tới thì trì hoãn bung 320 ms để thấy cú bật dậy.
- Nhạc: mọi lời gọi WinRT ở thread `nitrobar-media`, chờ sự kiện phiên; UI đọc `media::current()`.
  Cột sóng dùng `IAudioMeterInformation` (chỉ độ to, không phân tích tần số), `TIMER_BARS` 33 ms CHỈ khi
  đang phát + viên thu gọn + không toàn màn hình. Viên nới 200 -> 288 DIP bằng lò xo; cửa sổ luôn đủ
  rộng cho mức tối đa nên chỉ đổi region.
- KHI THỬ: kiểm tra người dùng có đang xem video / toàn màn hình không (không chỉ idle: xem video thì
  không có input). Đã một lần phát âm thanh thử đè lên video của người dùng.

### Bên trái viên lúc nghỉ, độ trễ thu

- Theo yêu cầu người dùng: có app đang ở foreground => icon NHỎ của app (20 DIP, `Icon::small` =
  5/6 cỡ icon, vẽ 1:1) thay Nitro, crossfade 200 ms; desktop (Progman / WorkerW) => Nitro; flyout /
  cửa sổ không có nút => giữ nguyên; Nitro đang ngủ => luôn hiện Nitro. Đổi icon / Nitro bằng MORPH
  (`island::icon_morph`, 360 ms: cũ co + xoay đi, mới nở + xoay vào cùng chỗ, nảy nhẹ; người dùng
  không muốn kiểu mờ chuyển). Lúc bung / thu, icon app đó BAY giữa viên và nút của nó
  (`App::flying_button`, `Scene::flight`, `ButtonView::hide_icon`).
- `collapse_delay_ms` mặc định 60 (người dùng muốn rời chuột là thu ngay). Ngoại lệ: đang mở xem
  trước thì 500 ms (đủ đi qua khe tới panel); bị chặn (menu, Start, kéo) thì kiểm tra lại mỗi
  >= 300 ms (không chạy nhịp 60 ms).

### Icon khay hệ thống (tray.rs, tray_panel.rs, render/tray.rs)

- Người dùng KHÔNG muốn taskbar gốc hiện lên (bản đầu cho nút "^" hiện taskbar: đã bỏ) => nitrobar tự
  vẽ icon khay.
- `tray.rs`: thread `nitrobar-tray` giữ một cửa sổ class `Shell_TrayWnd` (không bao giờ hiện) đứng
  TRƯỚC cửa sổ của Explorer => `Shell_NotifyIcon` gửi `WM_COPYDATA` (dwData 1) vào đó. Đọc rồi CHUYỂN
  TIẾP NGUYÊN VẸN mọi `WM_COPYDATA` (cả appbar), `WM_COMMAND`, `WM_USER..WM_APP` cho taskbar thật,
  trả đúng kết quả => Explorer vẫn biết đủ, thoát / crash không mất gì. Struct trên dây dùng handle
  32 bit. Sau khi tạo cửa sổ thì post `TaskbarCreated` cho cửa sổ của process khác (không tự nhận).
  Giữ vị trí đầu bằng hook foreground + show/reorder của process Explorer (không polling).
- MỌI chỗ cần taskbar THẬT phải dùng `tray::real_taskbar()` (cửa sổ có con `TrayNotifyWnd`), không
  dùng `FindWindowW("Shell_TrayWnd")` (sẽ ra cửa sổ giả, kể cả của instance khác khi
  `--restore-taskbar`). Script test cũng vậy. Đã đo: ẩn / trả taskbar vẫn đúng khi tray chạy.
- Chỉ chạy khi island đứng thay taskbar (`apply_native_taskbar`). Lọc icon loa / mạng / pin của
  Windows (GUID `7820AE7x-...`). Không lấy được: chỉ báo riêng của Windows (định vị, mic), app chạy
  quyền admin (UIPI chặn broadcast), app không nghe `TaskbarCreated` (cần khởi động lại app).
- Bấm: post callback cho app theo version (>=3 thêm `NIN_SELECT` / `WM_CONTEXTMENU`; >3 gói toạ độ +
  id), kèm `AllowSetForegroundWindow`. Chuột phải thì đóng bảng + thu dải; trái thì giữ bảng (còn
  double click). CHƯA thử bấm icon thật để mở menu.
- Giao diện theo yêu cầu người dùng (giống bảng overflow của Windows): hộp gọn, ô 40 DIP, lề 4, góc
  10, viền mờ 1 px; KHÔNG chữ trong bảng; tên icon (dòng đầu tooltip) nằm trong hộp nhỏ riêng PHÍA
  TRÊN, giữa ô đang hover. Cửa sổ rộng ra theo tên, bảng đứng yên (neo theo tâm bảng).
- Bảng: cửa sổ layered (`UpdateLayeredWindow`), ảnh do `Renderer::tray_panel` vẽ vào bitmap WIC; chuột
  báo về thanh qua `WM_APP_PANEL`; giữ dải mở qua `tray_hover`; `close_preview` đóng cả bảng.
- Đo (release): nghỉ 15.1-15.5 MB, sau khi mở bảng / dải 18.7-20.1 MB (CHẠM ngân sách 20 MB, cần giảm).

### Mở app

- `pins::launch*` chạy `ShellExecute` trên thread riêng (`AllowSetForegroundWindow` trước đó): trên UI
  thread nó làm thanh đứng hình. Khôi phục: overlay 220 ms rồi mới hiện cửa sổ thật (trước 380 ms).

### Lưới của dải (`island::strip_layout`, có test)

- Thứ tự: Start, icon app, vạch, trạng thái, giờ. `GRID_GAP` 16 DIP giữa các mép NHÌN THẤY; hai đầu cách
  panel đúng nửa chiều cao (đo trên màn hình: 26 / 26 px). Bề rộng phần trăm pin và giờ được ĐO bằng
  DirectWrite (`Renderer::strip_text`, lưu ở `App::strip_text`, đo ngoài lúc paint vì renderer bị lấy
  ra); đừng quay lại cột cố định (từng để dư ~24 DIP trước giờ).
- Start trong dải: đơn sắc theo màu chữ (tối trên kính sáng), zoom + plate như icon; bấm trái = phím
  Win, bấm phải = Win+X (`shell::open_power_menu`), không hiện menu của nitrobar.
- Mọi cửa sổ của mình đặt `DWMWA_BORDER_COLOR = NONE` (`theme::no_border`). Quét đường kẻ trên màn
  hình thật (nền phẳng, mọi trạng thái) không thấy dòng nào do nitrobar vẽ.

### Chuyển động bung / thu (island.rs, thuần hình học, có test)

- Viên nghỉ và dải CÙNG TÂM (`strip_layout` đặt tâm dải = tâm viên) => `morph_panel` giãn đối xứng.
  `Island::eased` là lò xo cả hai chiều (mở 520 ms, (24, 0.70); đóng 380 ms, (24, 0.86));
  `Island::squash` dẹt tối đa 4 DIP theo tốc độ (`island_squash`).
- Nội dung KHÔNG scale theo viên: đứng ở vị trí cuối, outline viên làm `geometricMask` của layer
  (chỉ khi đang chuyển động). `reveals`: mở = so le 25 ms mỗi bậc cách tâm, lăn từ phía tâm 8 DIP,
  xoay 15°→0, scale 0.6→1 + nảy; đóng = ngoài cùng trước. `rest_opacity`: nội dung nghỉ rời ngay khi mở,
  về khi dải gần hết (không chồng nhau, có test). Chữ (trạng thái, giờ) không xoay.
- Chụp chuỗi khung: bộ chụp offscreen ghi `open-*.bmp` / `close-*.bmp` mỗi 20 ms và in ms vẽ một khung
  (0.8-1.5 ms, ngân sách 16.6 ms). Chỉnh lò xo thì xem chuỗi này trước.
- CHƯA chạy trên compositor (Composition animation + surface vẽ sẵn). Đã đo: tạo D3D11 WARP + D2D
  device +1.3 MB private (chưa tính surface / lần vẽ đầu); device phần cứng +46 MB (vượt ngân sách).
  Nếu làm thì phải dùng WARP.

### Hiệu ứng thu nhỏ / khôi phục cửa sổ

- `HSHELL_GETMINRECT` (shell hook, wParam 5, lParam -> `SHELLHOOKINFO {hwnd, rc}`): Windows hỏi
  rect đích lúc thu nhỏ VÀ lúc khôi phục, gửi đồng bộ (app đang chờ) => trả lời ở nhánh stateless
  của `wndproc`, `App` bận thì dùng `MIN_RECT` (viên / cả thanh).
- Đã đo trên Win11 26300: cửa sổ shell hook nào trả lời trước thì thắng; nitrobar trả lời =>
  Explorer không được hỏi và Windows KHÔNG chạy hiệu ứng nào (cửa sổ biến mất / hiện ra ngay), dù
  rect ở đâu, to hay nhỏ. Không thể bắt hiệu ứng gốc bay vào island; bỏ qua (trả 0) thì hiệu ứng
  gốc bay về nút của taskbar gốc đang ẩn. Hệ quả tốt: KHÔNG cần tắt "Animate windows when minimizing"
  (không đụng cài đặt hệ thống). `[bar] window_animation = false` => không trả lời (hiệu ứng gốc).
- Windows hỏi SAU khi đổi trạng thái: thu nhỏ => `IsIconic` đã true; khôi phục => đã false.
- Thu nhỏ (`morph.rs`): KHÔNG làm gì nặng trong `GETMINRECT`: có DWM thumbnail của cửa sổ trong lúc nó
  đang thu nhỏ (kể cả chỉ `DwmRegisterThumbnail`) thì hiệu ứng gốc LẠI HIỆN (một bóng cửa sổ bay
  trước overlay, đã tái hiện). => post `WM_APP_MINIMIZED`, đăng ký thumbnail sau khi cửa sổ đã ẩn.
  Thumbnail của cửa sổ đã thu nhỏ vẫn có nội dung (khung cuối). Giá: cửa sổ biến mất ~1-2 frame
  (17-33 ms, đo bằng GetPixel) trước khi overlay hiện.
- Rect cửa sổ: `GetWindowPlacement` (toạ độ workspace = cộng góc work area của màn hình đó; trừ viền
  resize vô hình `SM_CXSIZEFRAME + SM_CXPADDEDBORDER`; `WPF_RESTORETOMAXIMIZED` => work area).
- Overlay `Morph`: cửa sổ layered, click xuyên qua, nền đen (class brush), cố định bằng hợp của
  đường đi; mỗi frame chỉ đổi region bo tròn (`CreateRoundRectRgn`, clip cả thumbnail => bo góc được,
  nhưng răng cưa), rect / opacity của thumbnail (mờ dần trên nền đen = tối dần), alpha cả cửa sổ.
  Nằm ngay dưới cửa sổ thanh (khối đen trượt xuống dưới viên). Ease in-out 400 ms; tới 78% thì viên
  bắt đầu nảy ("nuốt"). Giữ tỉ lệ ảnh trong khối.
- Khôi phục có hiệu ứng chỉ khi bấm nút trên thanh (`tasks::click_target` là cửa sổ đang thu nhỏ):
  overlay bung từ viên / nút (ease-out 380 ms), XONG mới `tasks::activate`. App cần ~50-250 ms để vẽ
  lại sau khi khôi phục; trong lúc đó cả cửa sổ lẫn thumbnail của nó trống (đo bằng GetPixel: 1 frame
  đen rồi trong suốt). Ẩn overlay ngay => lộ màn hình phía sau; giữ nguyên => lộ nền đen. Nên overlay
  mờ dần 160 ms (`LINGER_FADE`), ở lại ít nhất tới khi cửa sổ hết iconic, tối đa 700 ms. Còn 1 frame
  tối không tránh được. Không ẩn được cửa sổ của process khác trong lúc nó hiện ra (cloak chỉ
  cho cửa sổ của mình, layered thì có thể làm hỏng app DirectX) => khôi phục từ nơi khác (Alt+Tab,
  app tự mở) chỉ có viên nảy. `own_restore` bỏ qua `GETMINRECT` của lần khôi phục do mình làm.
- Tôn trọng cài đặt Windows: `SPI_GETANIMATION` (iMinAnimate) / `SPI_GETCLIENTAREAANIMATION` tắt => không
  có overlay. Không hiệu ứng khi có app toàn màn hình.
- Đo: hiệu ứng thu nhỏ + khôi phục (kèm 2 lần nảy) ~60-80 ms CPU; đứng yên 0 ms/5 s.

## Bất biến an toàn: taskbar gốc PHẢI hiện lại

- Khi taskbar gốc đang ẩn và Start / Quick Settings mở, Explorer hiện lại taskbar; ẩn nó ngay lúc
  đó làm Start đóng lập tức (lỗi có từ GĐ1 với `--hide-taskbar`). Hook `on_object_show` bỏ qua khi
  flyout đang ở foreground; `taskbar::foreground_changed` ẩn lại khi flyout đóng.

- Mọi đường thoát đều gọi `safety::emergency_cleanup()` (idempotent, gọi được từ thread bất kỳ):
  thoát bình thường, WM_DESTROY, WM_ENDSESSION, panic hook, Ctrl+C / đóng console, SEH filter.
- `TerminateProcess` không bắt được: trạng thái auto-hide gốc được ghi vào
  `%LOCALAPPDATA%\nitrobar\taskbar-state`; lần chạy sau `recover_after_crash()` tự phục hồi,
  hoặc chạy `--restore-taskbar`.
- Code mới đụng tới taskbar gốc phải giữ các bất biến trên; test bằng cách kill cứng process.
- Release dùng `panic = "abort"`; panic hook vẫn chạy trước khi abort.

## Phong cách code

- Edition 2024, `unsafe_op_in_unsafe_fn = deny`: mỗi lời gọi Win32 nằm trong `unsafe {}` nhỏ nhất có thể.
- Comment và tên trong code bằng tiếng Anh; tài liệu dự án (file này, README) bằng tiếng Việt.
- Lỗi không nghiêm trọng (vẽ hỏng, API phụ trả lỗi) thì `debug_log!` rồi bỏ qua; không `unwrap()`
  trên kết quả Win32.
- Kết quả Win32 cố tình bỏ qua viết `let _ = ...;`.
- Truyền con trỏ cho Win32 bằng `&raw const x` / `&raw mut x` (biến), `&CONST` cho hằng.
- Không dùng `Set-Content`/`Out-File` của PowerShell 5.1 để ghi file nguồn: chúng chèn BOM.

## Kiểm thử thủ công

- Mỗi lần chạy thử phải kết thúc bằng `nitrobar.exe --quit` (hoặc kiểm tra taskbar gốc hiện lại).
- Mở menu rồi bấm phím trong CÙNG một script (menu tự đóng giữa hai lần gọi tool, phím rơi vào
  cửa sổ khác). Vị trí nút tính bằng công thức layout (căn giữa, 44 + 4 DIP), không dò pixel.
- Không đọc rồi ghi lại config.toml bằng `Get-Content` của PowerShell 5.1: nó đọc UTF-8 không BOM
  như ANSI, ghi lại là hỏng chữ tiếng Việt. Dùng `[IO.File]::ReadAllText` / `WriteAllText`.
- Thử tương tác bằng một cửa sổ riêng (ví dụ WinForms tiêu đề `nitrobar-test`), KHÔNG bấm vào
  cửa sổ thật của người dùng. Notepad Win11 là app đóng gói và tự mở lại tab cũ: không dùng để test.
- Gửi click giả bằng `PostMessage(WM_LBUTTONDOWN/UP)` vào cửa sổ thanh; `WM_CONTEXTMENU` cho menu.
- Dò vị trí nút theo pixel chỉ đúng khi nền đặc: khi test, tạm đặt `background = "solid"` +
  `background = "#202020"` trong config.toml (áp dụng trực tiếp) rồi trả lại file cũ.
- Trước khi chọn mục menu bằng phím, chụp ảnh menu để chắc đúng menu: một lần chọn nhầm
  "Mở file cấu hình" đã mở tab config.toml trong VS Code của người dùng.

## Rủi ro / phần khó đã biết

- **System tray (icon khay hệ thống)**: không có API công khai. Phải tạo cửa sổ `Shell_TrayWnd`
  giả để nhận `Shell_NotifyIcon` (xung đột với Explorer) hoặc đọc toolbar của Explorer (dễ vỡ
  qua các bản Windows 11). Ngoài phạm vi 5 giai đoạn; sẽ đánh giá riêng.
- **Explorer tự hiện lại taskbar** (mở Start, đổi màn hình): đã có WinEvent hook EVENT_OBJECT_SHOW
  ẩn lại; cần theo dõi khi Windows cập nhật.
- **Mở Start / Quick Settings**: không có API chính thức; dùng giả lập phím Win / Win+A / Win+N
  (`SendInput`, chỉ khi người dùng vừa bấm vào thanh).
- **Nền trong suốt**: accent policy không công khai; nếu mất, blur/acrylic tự về clear.
- **Icon app UWP/packaged**: đã giải quyết bằng AUMID -> `shell:AppsFolder` image factory.
- **Đa màn hình**: thanh chỉ ở màn hình chính.
- **Xem trước cửa sổ khi hover** (thumbnail): chưa làm; `DwmRegisterThumbnail` là hướng đi.

- Màu island: `island_color` = wallpaper (`wallpaper.rs`: WIC thu nhỏ 48 px trên luồng nền, màu chủ đạo theo bin hue, tính lại khi `WM_SETTINGCHANGE` wParam `SPI_SETDESKWALLPAPER`) | accent | #RRGGBB, `island_opacity` = độ đậm tint; chữ tối/sáng tính MỘT lần từ màu. Menu "Màu island" ghi config qua `config::set_bar_value` (sửa tại chỗ, giữ chú thích) rồi file watcher áp dụng; "Tự chọn" dùng `ChooseColorW` ngoài borrow của App.
