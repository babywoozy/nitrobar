# nitrobar — quy ước dự án

Taskbar thay thế cho Windows 11, viết bằng Rust. Ưu tiên số 1: **nhẹ và nhanh**.

## Lệnh thường dùng

```powershell
cargo build --release                               # build chính thức
cargo clippy --release --all-targets -- -D warnings # phải sạch trước khi commit
cargo fmt                                           # format trước khi commit
.\target\release\nitrobar.exe [--hide-taskbar]      # chạy
.\target\release\nitrobar.exe --quit                # tắt instance đang chạy
.\target\release\nitrobar.exe --restore-taskbar     # cứu taskbar gốc nếu bị kẹt ẩn
```

Bản debug có console (thấy log `debug_log!`); bản release là GUI subsystem, không có console.

## Ngân sách hiệu năng (bắt buộc)

- RAM: private bytes < 10 MB, working set < 20 MB. Hiện tại (giai đoạn 1): ~4.6 MB / ~17 MB.
- CPU: 0 ms khi không có sự kiện. Đo lại sau mỗi giai đoạn (Process.TotalProcessorTime trong 5 giây).
- Không polling. Mọi cập nhật đến từ sự kiện: WinEvent hook, shell hook, appbar callback,
  WM_SETTINGCHANGE, notification COM... Timer chỉ dùng khi không có sự kiện tương ứng, và
  phải là one-shot căn theo thời điểm cần (ví dụ đồng hồ: đúng biên phút), không phải tick đều.
- Direct2D dùng **software render target** (`D2D1_RENDER_TARGET_TYPE_SOFTWARE`). Bản GPU
  kéo theo D3D11 + driver, tốn thêm ~30 MB private. Chỉ đổi nếu có số đo chứng minh.

## Phụ thuộc

- Chỉ dùng crate `windows` (windows-rs). Không dùng framework GUI (egui, Tauri, webview, winit...).
- Thêm crate mới phải có lý do rõ ràng (ví dụ `toml` + `serde` cho giai đoạn 5) và ghi vào đây.
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
```

Dự kiến thêm: `windows_list.rs` + `shell_hook.rs` (GĐ2), `icons.rs` (GĐ2), `start.rs` /
`backdrop.rs` (GĐ3), `status/{network,volume,battery}.rs` (GĐ4), `config.rs` + `menu.rs` +
`pins.rs` (GĐ5).

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

## Rủi ro / phần khó đã biết

- **System tray (icon khay hệ thống)**: không có API công khai. Phải tạo cửa sổ `Shell_TrayWnd`
  giả để nhận `Shell_NotifyIcon` (xung đột với Explorer) hoặc đọc toolbar của Explorer (dễ vỡ
  qua các bản Windows 11). Ngoài phạm vi 5 giai đoạn; sẽ đánh giá riêng.
- **Explorer tự hiện lại taskbar** (mở Start, đổi màn hình): đã có WinEvent hook EVENT_OBJECT_SHOW
  ẩn lại; cần theo dõi khi Windows cập nhật.
- **Mở Start / Quick Settings**: không có API chính thức; dùng giả lập phím Win / Win+A.
- **Mica/Acrylic** cho cửa sổ popup: `DWMWA_SYSTEMBACKDROP_TYPE` cần Windows 11 22H2+, và
  phải vẽ nền có alpha (software target hiện tại hỗ trợ premultiplied alpha).
- **Icon thật của app UWP/packaged**: `WM_GETICON` không có; cần `SHGetPropertyStoreForWindow`
  + AppUserModelID.
- **Đa màn hình**: giai đoạn 1 chỉ màn hình chính.
