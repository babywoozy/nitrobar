# nitrobar

Taskbar thay thế cho Windows 11, viết bằng Rust + Win32 thuần (Direct2D/DirectWrite).
Nhẹ (~9 MB RAM riêng, file exe ~0,5 MB), chạy hoàn toàn theo sự kiện (0% CPU khi rảnh).

## Tính năng

- **Chế độ island (mặc định)**: một viên đen bo tròn nổi ở giữa mép dưới màn hình (200×40 DIP, cách
  mép 10 DIP, có bóng đổ mềm). Bên trái là icon của app đang active (đổi app thì icon mờ chuyển sang
  icon mới), bên phải là giờ. Thanh không chiếm chỗ (cửa sổ phóng to tràn xuống tận đáy; taskbar gốc
  được ẩn).
  - Rê chuột vào viên hoặc chạm mép dưới ở đoạn giữa (rộng bằng viên + 40 DIP mỗi bên; phần còn lại
    của mép, ví dụ ô chat của app khác, không làm gì) thì sau ~30 ms viên giãn ngang (~200 ms, lò xo nhẹ) thành dải
    icon các app (app ghim + đang chạy, gom theo app), vẫn là một khối bo tròn nổi, rộng vừa đủ. Vạch
    ngắn dưới app đang mở, vạch dài màu nhấn dưới app đang active, chấm mờ dưới app đã thu nhỏ hết
    cửa sổ, nền cam khi app cần chú ý. Chỉ icon đang có chuột phóng to nhẹ (1,2 lần), các icon khác giữ nguyên. Wifi / âm lượng
    / pin / đồng hồ nằm cuối dải, ngăn cách bằng một vạch mờ. Rời chuột ~600 ms thì thu về viên.
  - Bấm icon trên dải: app đang thu nhỏ thì cửa sổ tỏa ra từ đúng icon đó (khối tròn đen cỡ icon lớn
    dần, sáng dần thành cửa sổ thật) rồi dải thu lại; app đang active thì cửa sổ co ngược vào đúng
    icon và viên nảy lên với icon app; app đang mở nhưng ở sau thì được đưa lên trước và icon nảy
    nhẹ; app chưa chạy (ghim) thì được mở; app có nhiều cửa sổ thì hiện ảnh xem trước (live) kèm
    tiêu đề của từng cửa sổ phía trên dải để chọn, bấm lại icon để đóng.
  - Nhấn phím Win (mở Start) cũng làm thanh bung ra. Không thu lại khi đang mở menu chuột phải, Start,
    Quick Settings, trung tâm thông báo hay đang kéo thả. Đang chơi game / xem video toàn màn hình thì
    viên ẩn đi và chạm mép không làm thanh bung ra. Dải island không có nút Start (dùng phím Win).
- **Chế độ full**: thanh luôn hiện ở đáy (hoặc đỉnh), đăng ký AppBar: cửa sổ phóng to không đè lên thanh.
- Nút Start (mở Start menu của Windows), app đang chạy gom nhóm theo ứng dụng với icon thật.
  Vạch ngắn = đang chạy, vạch dài màu nhấn = đang mở, nền cam = app cần chú ý.
- Icon nét như taskbar gốc: lấy đúng cỡ pixel theo DPI, giữ kênh alpha (viền mượt), không phóng to
  icon nhỏ; app Store lấy logo từ package.
- Bấm app để chuyển tới / thu nhỏ; app nhiều cửa sổ thì bấm để xoay vòng; chuột giữa mở thêm cửa sổ mới.
- App ghim lấy thẳng từ taskbar gốc của Windows, đúng thứ tự (cả app Store), tự cập nhật khi
  bạn ghim / bỏ ghim trên taskbar gốc. App ghim chưa chạy hiện icon không vạch, bấm để mở;
  app ghim đang chạy dùng chung một nút. Ghim / bỏ ghim thêm bằng chuột phải.
- Wifi, âm lượng, pin (%): bấm để mở Quick Settings. Bấm đồng hồ mở trung tâm thông báo.
- Nền trong suốt hoàn toàn (mặc định, như TranslucentTB "clear": thấy rõ hình nền), hoặc mờ (blur),
  acrylic, đặc. Chữ và icon có bóng đổ nhẹ nên đọc được trên mọi hình nền. Theme sáng/tối và màu
  nhấn theo Windows, menu tối, hỗ trợ DPI cao.
- Tuỳ chọn ẩn taskbar gốc; taskbar gốc luôn được trả lại khi thoát, crash hoặc tắt máy.

## Cài đặt

Cần Windows 10/11 và Rust 1.88+.

```powershell
git clone https://github.com/babywoozy/nitrobar
cd nitrobar
cargo build --release
.\target\release\nitrobar.exe
```

Tự chạy khi đăng nhập: nhấn Win+R, gõ `shell:startup`, tạo shortcut tới `nitrobar.exe` trong thư mục đó.

## Dùng

```powershell
nitrobar.exe                    # chạy (island: ẩn taskbar gốc; full: nằm ngay trên taskbar gốc)
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
background = "clear"         # "clear" (trong suốt hẳn), "blur" (mờ), "acrylic" hoặc "solid" (đặc)

mode = "island"              # "island" (thu gọn, rê chuột thì bung ra) hoặc "full" (luôn hiện)
island_width = 200           # cỡ viên lúc nghỉ (DIP, 40..600)
island_height = 40           # (DIP, 4..80)
island_gap = 10              # khoảng cách tới mép màn hình (DIP, 0..60)
expand_delay_ms = 30         # rê chuột vào bao lâu thì bung ra (0 = ngay lập tức)
collapse_delay_ms = 600      # rời chuột bao lâu thì thu lại
island_show_clock = true     # hiện giờ ở bên phải viên (bên trái là icon app đang active)
window_animation = true      # thu nhỏ cửa sổ "chui" vào island; false = hiệu ứng của Windows

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
- [x] Ghim theo taskbar gốc, icon nét, nền trong suốt, sửa lỗi thanh đen
- [x] Chế độ island: viên thu gọn, bung ra khi rê chuột / chạm mép / nhấn Win, có hiệu ứng
- [x] Hiệu ứng thu nhỏ / khôi phục cửa sổ vào island (DWM thumbnail + viên nảy)
- [x] Thiết kế island kiểu Dynamic Island: viên nghỉ (icon app + giờ), dải icon khi hover (zoom kiểu
  dock), bấm icon (tỏa ra / co vào icon, nhảy icon, xem trước nhiều cửa sổ)

Chưa có: khay hệ thống (icon app chạy nền), nhiều màn hình (thanh chỉ ở màn hình chính). Ở chế độ
island, `background` và `[colors]` không áp dụng: viên luôn đen (nền trong suốt hẳn không đọc được
khi thanh đè lên cửa sổ app); chúng chỉ dùng cho chế độ `full`. Hiệu ứng
thu nhỏ: góc bo của ảnh cửa sổ hơi răng cưa (DWM không cho bo góc thumbnail, phải cắt bằng region),
cửa sổ biến mất ~1-2 khung hình trước khi hiệu ứng bắt đầu, và khi khôi phục có thể thấy một khung
tối lúc app vẽ lại.
