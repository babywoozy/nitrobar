# nitrobar

Taskbar thay thế cho Windows 11, viết bằng Rust + Win32 thuần (Direct2D/DirectWrite).
Nhẹ (~9 MB private memory), chạy theo sự kiện (0% CPU khi rảnh).

## Trạng thái

- [x] Giai đoạn 1: thanh ở đáy màn hình, đăng ký AppBar, đồng hồ, ẩn/khôi phục taskbar gốc
- [x] Giai đoạn 2: danh sách app đang chạy (gom nhóm theo app), icon thật, bấm để chuyển / thu nhỏ, menu chuột phải (Đóng cửa sổ / Thoát)
- [x] Giai đoạn 3: nút Start, hover / nhấn, nút bo góc, nền acrylic, theme sáng/tối + màu accent, menu tối
- [ ] Giai đoạn 4: wifi, âm lượng, pin, mở Quick Settings
- [ ] Giai đoạn 5: ghim app, menu chuột phải, cấu hình TOML

## Dùng

```powershell
cargo build --release
.\target\release\nitrobar.exe                    # chạy, giữ taskbar gốc
.\target\release\nitrobar.exe --hide-taskbar     # chạy và ẩn taskbar gốc
.\target\release\nitrobar.exe --quit             # tắt
.\target\release\nitrobar.exe --restore-taskbar  # nếu taskbar gốc bị kẹt ẩn
```

Yêu cầu: Windows 10 1703+ (khuyến nghị Windows 11), Rust 1.88+.
