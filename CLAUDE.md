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

- RAM: private bytes < 20 MB (bắt buộc), mục tiêu < 10 MB. Working set tính cả trang DLL hệ thống dùng chung nên chỉ để tham khảo. Đo được: GĐ1 4.6 MB / 17 MB; GĐ2 9.1 MB / 31 MB (WIC + shell COM cho icon); GĐ3 9.3 MB / 31 MB; GĐ4 9.5 MB / 32 MB; GĐ5 9.2 MB / 33 MB, exe 486 KB.
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
  icons.rs    icon thật -> IWICBitmap (AUMID/AppsFolder, WM_GETICON, icon file exe)
  menu.rs     menu chuột phải (TrackPopupMenu, chạy NGOÀI borrow của App)
  theme.rs    sáng/tối + màu accent từ registry, Palette, nền acrylic (DWM), menu tối
  shell.rs    mở UI của Windows bằng phím tắt giả lập (Start = Win, Quick Settings = Win+A,
              thông báo = Win+N)
  status/     wifi / âm lượng / pin: mod.rs (gộp thông báo, StatusView), network.rs,
              volume.rs (COM callback), battery.rs
  config.rs   %APPDATA%\nitrobar\config.toml: parse + clamp, file mặc định có chú thích, màu
  pins.rs     %APPDATA%\nitrobar\pins.toml (app tự ghi), mở app qua shell:AppsFolder / exe
```

### Cấu hình và ghim (config.rs, pins.rs)

- `DEFAULT_CONFIG` (có chú thích) phải parse ra đúng `Config::default()`: có unit test. Thêm
  khoá mới thì sửa cả hai, cả README.
- Tải lại trực tiếp: `FindFirstChangeNotificationW` trên thư mục config, handle được chờ ngay
  trong message loop (`MsgWaitForMultipleObjects`), không có thread riêng. File lỗi -> giữ
  config cũ, lỗi hiện thành mục mờ trong menu chuột phải.
- Kích thước lấy từ `layout::Metrics` (theo config), không dùng hằng số cố định cho thanh/nút/icon.
- Pins tách khỏi config.toml để app ghi lại mà không xoá chú thích của người dùng.
- Thứ tự nút: app ghim (theo thứ tự ghim, gộp với nhóm đang chạy cùng key) rồi app đang chạy
  chưa ghim. `Hit::Task(i)` là chỉ số trong danh sách gộp này (`App::items()`).

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
- Acrylic = `DwmExtendFrameIntoClientArea(-1)` + `DWMWA_SYSTEMBACKDROP_TYPE = TRANSIENTWINDOW`,
  vẽ nền trong suốt + lớp tint `palette.background` có alpha. DWM chỉ vẽ backdrop cho cửa sổ
  "active", nên wndproc luôn trả `WM_NCACTIVATE(TRUE)` (thanh là WS_EX_NOACTIVATE). Bỏ trick này
  là nền thành màu xám đặc.
- Khi trong suốt: text antialias GRAYSCALE (ClearType cần nền đặc).
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
- Thử tương tác bằng một cửa sổ riêng (ví dụ WinForms tiêu đề `nitrobar-test`), KHÔNG bấm vào
  cửa sổ thật của người dùng. Notepad Win11 là app đóng gói và tự mở lại tab cũ: không dùng để test.
- Gửi click giả bằng `PostMessage(WM_LBUTTONDOWN/UP)` vào cửa sổ thanh; `WM_CONTEXTMENU` cho menu.
- Dò vị trí nút theo pixel chỉ đúng khi nền đặc: khi test, tạm đặt `acrylic = false` +
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
- **Acrylic**: `DWMWA_SYSTEMBACKDROP_TYPE` cần Windows 11 22H2+; máy cũ hơn tự về nền đặc.
- **Icon app UWP/packaged**: đã giải quyết bằng AUMID -> `shell:AppsFolder` image factory.
- **Đa màn hình**: thanh chỉ ở màn hình chính.
- **Xem trước cửa sổ khi hover** (thumbnail): chưa làm; `DwmRegisterThumbnail` là hướng đi.
