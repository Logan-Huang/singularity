use image::RgbaImage;
use windows::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, SelectObject,
    },
    Storage::Xps::{PRINT_WINDOW_FLAGS, PW_CLIENTONLY, PrintWindow},
    UI::WindowsAndMessaging::GetClientRect,
};

/// Undocumented but in winuser.h since Windows 8.1; makes `PrintWindow` grab
/// DWM-composed content (DirectX apps, XAML apps) instead of only asking the
/// window to paint itself via WM_PRINT, which many modern apps ignore.
const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(2);

/// Screenshots one window's client area, even if it is behind other windows.
pub fn capture_window(window: HWND) -> Option<RgbaImage> {
    unsafe {
        let mut client_rect = RECT::default();
        GetClientRect(window, &raw mut client_rect).ok()?;
        let width = client_rect.right - client_rect.left;
        let height = client_rect.bottom - client_rect.top;
        if width <= 0 || height <= 0 {
            return None;
        }

        let hdc = CreateCompatibleDC(None);

        let bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // negative height = top-down row order, matching RgbaImage
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = core::ptr::null_mut();
        let Ok(bitmap) = CreateDIBSection(
            Some(hdc),
            &raw const bitmap_info,
            DIB_RGB_COLORS,
            &raw mut bits,
            None,
            0,
        ) else {
            let _ = DeleteDC(hdc);
            return None;
        };
        let previous_object = SelectObject(hdc, bitmap.into());

        let printed = PrintWindow(
            window,
            hdc,
            PRINT_WINDOW_FLAGS(PW_CLIENTONLY.0 | PW_RENDERFULLCONTENT.0),
        )
        .as_bool();
        let _ = GdiFlush();

        let image = if printed {
            let bgra =
                core::slice::from_raw_parts(bits.cast::<u8>(), (width * height * 4) as usize);
            let rgba = bgra
                .chunks_exact(4)
                // GDI hands back BGRA, and its alpha channel is garbage for
                // GDI-rendered content, so force opaque
                .flat_map(|pixel| [pixel[2], pixel[1], pixel[0], 255])
                .collect();
            RgbaImage::from_vec(width as u32, height as u32, rgba)
        } else {
            None
        };

        SelectObject(hdc, previous_object);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(hdc);

        image
    }
}
