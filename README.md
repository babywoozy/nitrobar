# nitrobar

Taskbar thay thế cho Windows 11, viết bằng Rust + Win32 thuần (Direct2D/DirectWrite).
Nhẹ (~9 MB RAM riêng, file exe ~0,5 MB), chạy hoàn toàn theo sự kiện (0% CPU khi rảnh).

## Tính năng

- Thanh ở đáy (hoặc đỉnh) màn hình, đăng ký AppBar: cửa sổ phóng to không đè lên thanh.
- Nút Start (mở Start menu của Windows), app đang chạy gom nhóm theo ứng dụng với icon thật.
  Vạch ngắn = đang chạy, vạch dài màu nhấn = đang mở, nền cam = app cần chú ý.
- Bấm app để chuyển tới / thu nhỏ; app nhiều cửa sổ thì bấm để xoay vòng; chuột giữa mở thêm cửa sổ mới.
- App ghim lấy thẳng từ taskbar gốc của Windows, đúng thứ tự (cả app Store), tự cập nhật khi
  bạn ghim / bỏ ghim trên taskbar gốc. App ghim chưa chạy hiện icon không vạch, bấm để mở;
  app ghim đang chạy dùng chung một nút. Ghim / bỏ ghim thêm bằng chuột phải.
- Wifi, âm lượng, pin (%): bấm để mở Quick Settings. Bấm đồng hồ mở trung tâm thông báo.
- Nền trong mờ acrylic, theme sáng/tối và màu nhấn theo Windows, menu tối, hỗ trợ DPI cao.
- Tuỳ chọn ẩn taskbar gốc; taskbar gốc luôn được trả lại khi thoát, crash hoặc tắt máy.

## Cài đặt

Cần Windows 11 (22H2+ để có nền acrylic; Windows 10 vẫn chạy với nền đặc) và Rust 1.88+.

```powershell
git clone https://github.com/babywoozy/nitrobar
cd nitrobar
cargo build --release
.\target\release\nitrobar.exe
```

Tự chạy khi đăng nhập: nhấn Win+R, gõ `shell:startup`, tạo shortcut tới `nitrobar.exe` trong thư mục đó.

## Dùng

```powershell
nitrobar.exe                    # chạy (giữ taskbar gốc, nitrobar nằm ngay trên nó)
nitrobar.exe --hide-taskbar     # chạy và ẩn taskbar gốc (hoặc đặt hide_native_taskbar = true)
nitrobar.exe --quit             # tắt (hoặc chuột phải trên thanh > Thoát nitrobar)
nitrobar.exe --restore-taskbar  # cứu taskbar gốc nếu bị kẹt ẩn (sau khi bị End task)
```

Chuột phải trên thanh:

| Vị trí | Mục trong menu |
|---|---|
| Trên một app | Ghim vào thanh / Bỏ ghim khỏi thanh, Đóng cửa sổ, Mở file cấu hình, Thoát nitrobar |
| Chỗ trống | Mở file cấu hình, Thoát nitrobar |

## Cấu hình

File `%APPDATA%\nitrobar\config.toml` được tạo sẵn ở lần chạy đầu (chuột phải > Mở file cấu hình).
Lưu file là thanh áp dụng ngay, không cần khởi động lại. Nếu file lỗi, thanh giữ cấu hình cũ
và hiện dòng báo lỗi trong menu chuột phải.

```toml
[bar]
position = "bottom"          # "bottom" hoặc "top"
height = 48                  # chiều cao thanh (DIP, 32..96)
icon_size = 24               # cỡ icon app (DIP, 16..48)
alignment = "center"         # "center" (giống Windows 11) hoặc "left"
hide_native_taskbar = false  # ẩn taskbar gốc của Windows khi nitrobar chạy
theme = "auto"               # "auto" (theo Windows), "dark" hoặc "light"
acrylic = true               # nền trong mờ (Windows 11 22H2 trở lên)

[colors]                     # để trống = theo theme Windows; "#RRGGBB" hoặc "#RRGGBBAA"
background = ""
text = ""
accent = ""                  # màu vạch của app đang mở

[clock]
show_date = true

[status]
show_battery_percent = true
```

### App ghim

Danh sách ghim = app ghim trên taskbar gốc của Windows (đọc trực tiếp, đúng thứ tự) + app bạn
ghim thêm trong nitrobar. nitrobar không sửa danh sách ghim của Windows; thay đổi của bạn lưu ở
`%APPDATA%\nitrobar\pins.toml` (nitrobar tự ghi):

- `[[pin]]`: app ghim thêm trong nitrobar (đứng sau app ghim của Windows).
- `[[hide]]`: app ghim của Windows mà bạn đã bỏ ghim trong nitrobar. Ghim lại thì app trở về
  đúng vị trí cũ.

## Trạng thái

- [x] Giai đoạn 1: thanh ở đáy màn hình, đăng ký AppBar, đồng hồ, ẩn/khôi phục taskbar gốc
- [x] Giai đoạn 2: danh sách app đang chạy (gom nhóm theo app), icon thật, bấm để chuyển / thu nhỏ, menu chuột phải (Đóng cửa sổ / Thoát)
- [x] Giai đoạn 3: nút Start, hover / nhấn, nút bo góc, nền acrylic, theme sáng/tối + màu accent, menu tối
- [x] Giai đoạn 4: wifi, âm lượng, pin (%), bấm mở Quick Settings; đồng hồ mở trung tâm thông báo
- [x] Giai đoạn 5: ghim app, menu chuột phải đầy đủ, file cấu hình TOML áp dụng trực tiếp

Chưa có: khay hệ thống (icon app chạy nền), xem trước cửa sổ khi hover, nhiều màn hình
(thanh chỉ ở màn hình chính).
