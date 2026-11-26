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

- RAM: private bytes < 20 MB (bắt buộc), mục tiêu < 10 MB. Working set tính cả trang DLL hệ thống dùng chung nên chỉ để tham khảo. Đo được: GĐ1 4.6 MB / 17 MB; GĐ2 9.1 MB / 31 MB (WIC + shell COM cho icon); GĐ3 9.3 MB / 31 MB; GĐ4 9.5 MB / 32 MB; GĐ5 9.2 MB / 33 MB, exe 486 KB; ghim từ Windows 12.1 MB / 48 MB (property system của shell); icon nét 12.0 MB; nền clear 11.5 MB (sau khi đổi qua lại 4 chế độ: 14.5 MB); chống thanh đen 12.6 MB.
- CPU: 0 ms khi không có sự kiện. Đo lại sau mỗi giai đoạn (Process.TotalProcessorTime trong 5 giây).
- Không polling. Mọi cập nhật đến từ sự kiện: WinEvent hook, shell hook, appbar callback,
  WM_SETTINGCHANGE, notification COM... Timer chỉ dùng khi không có sự kiện tương ứng, và
  phải là one-shot căn theo thời điểm cần (ví dụ đồng hồ: đúng biên phút), không phải tick đều.
- Direct2D dùng **software render target** (`D2D1_RENDER_TARGET_TYPE_SOFTWARE`). Bản GPU
  kéo theo D3D11 + driver, tốn thêm ~30 MB private. Chỉ đổi nếu có số đo chứng minh.

## Phụ thuộc

- Crate đang dùng: `windows` (Win32), `windows-core` (cho `#[implement]`), `serde` (derive) +
  `toml` (tắt default features) cho config.toml / pins.toml. Không dùng framework GUI (egui,
  Tauri, webview, winit...).
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

## Bất biến an toàn: taskbar gốc PHẢI hiện lại

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
