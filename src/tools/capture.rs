//! Снимок экрана через GDI: весь виртуальный рабочий стол (все мониторы) или его часть.

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject, CAPTUREBLT,
    DIB_RGB_COLORS, HGDIOBJ, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetCursorInfo, GetIconInfo, GetSystemMetrics, CURSORINFO, CURSOR_SHOWING, DI_NORMAL, ICONINFO,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

/// Картинка в BGRA (непрозрачная), сверху вниз, с положением на экране.
#[derive(Clone)]
pub struct Shot {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    pub bgra: Vec<u8>,
}

impl Shot {
    pub fn crop(&self, rect: RECT) -> Shot {
        let x0 = (rect.left - self.x).clamp(0, self.w as i32) as u32;
        let y0 = (rect.top - self.y).clamp(0, self.h as i32) as u32;
        let x1 = (rect.right - self.x).clamp(0, self.w as i32) as u32;
        let y1 = (rect.bottom - self.y).clamp(0, self.h as i32) as u32;
        let (w, h) = (x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1));
        let mut out = vec![0u8; (w * h * 4) as usize];
        for row in 0..h {
            let sy = (y0 + row).min(self.h - 1);
            let src = ((sy * self.w + x0) * 4) as usize;
            let len = (w.min(self.w - x0) * 4) as usize;
            let dst = (row * w * 4) as usize;
            out[dst..dst + len].copy_from_slice(&self.bgra[src..src + len]);
        }
        Shot { x: self.x + x0 as i32, y: self.y + y0 as i32, w, h, bgra: out }
    }

    pub fn to_rgba(&self) -> image::RgbaImage {
        let mut data = self.bgra.clone();
        for p in data.chunks_exact_mut(4) {
            p.swap(0, 2);
            p[3] = 255;
        }
        image::RgbaImage::from_raw(self.w, self.h, data).unwrap_or_default()
    }

    pub fn png(&self) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        let _ = self.to_rgba().write_to(&mut out, image::ImageFormat::Png);
        out.into_inner()
    }

    /// Цвет точки на экране.
    pub fn pixel(&self, sx: i32, sy: i32) -> Option<[u8; 3]> {
        let x = sx - self.x;
        let y = sy - self.y;
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return None;
        }
        let i = ((y as u32 * self.w + x as u32) * 4) as usize;
        Some([self.bgra[i + 2], self.bgra[i + 1], self.bgra[i]])
    }
}

pub fn virtual_screen() -> RECT {
    unsafe {
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        RECT { left: x, top: y, right: x + GetSystemMetrics(SM_CXVIRTUALSCREEN), bottom: y + GetSystemMetrics(SM_CYVIRTUALSCREEN) }
    }
}

/// Снимок прямоугольника экрана. cursor — дорисовать указатель мыши (для записи).
pub fn grab(rect: RECT, cursor: bool) -> Option<Shot> {
    let w = (rect.right - rect.left).max(1);
    let h = (rect.bottom - rect.top).max(1);
    unsafe {
        let screen = GetDC(HWND::default());
        let mem = CreateCompatibleDC(screen);
        let info = crate::wnd::bitmap_info(w, h);
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let Ok(bitmap) = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(mem);
            ReleaseDC(HWND::default(), screen);
            return None;
        };
        let old = SelectObject(mem, HGDIOBJ(bitmap.0));
        let _ = BitBlt(mem, 0, 0, w, h, screen, rect.left, rect.top, SRCCOPY | CAPTUREBLT);
        if cursor {
            draw_cursor(mem, rect.left, rect.top);
        }
        let len = (w * h * 4) as usize;
        let mut bgra = vec![0u8; len];
        if !bits.is_null() {
            std::ptr::copy_nonoverlapping(bits as *const u8, bgra.as_mut_ptr(), len);
        }
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(mem);
        ReleaseDC(HWND::default(), screen);
        for p in bgra.chunks_exact_mut(4) {
            p[3] = 255;
        }
        Some(Shot { x: rect.left, y: rect.top, w: w as u32, h: h as u32, bgra })
    }
}

/// Захват прямо в готовый буфер (запись экрана: без выделения памяти на каждый кадр).
pub struct Grabber {
    rect: RECT,
    mem: windows::Win32::Graphics::Gdi::HDC,
    bitmap: windows::Win32::Graphics::Gdi::HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    pub w: u32,
    pub h: u32,
}

impl Grabber {
    pub fn new(rect: RECT) -> Option<Self> {
        let w = (rect.right - rect.left).max(2);
        let h = (rect.bottom - rect.top).max(2);
        unsafe {
            let screen = GetDC(HWND::default());
            let mem = CreateCompatibleDC(screen);
            let info = crate::wnd::bitmap_info(w, h);
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let bitmap = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok();
            ReleaseDC(HWND::default(), screen);
            let Some(bitmap) = bitmap.filter(|_| !bits.is_null()) else {
                let _ = DeleteDC(mem);
                return None;
            };
            let old = SelectObject(mem, HGDIOBJ(bitmap.0));
            Some(Self { rect, mem, bitmap, old, bits: bits as *mut u8, w: w as u32, h: h as u32 })
        }
    }

    /// Кадр: BGRA сверху вниз, w*h*4 байт. Буфер живёт до следующего кадра.
    pub fn frame(&mut self) -> &[u8] {
        unsafe {
            let screen = GetDC(HWND::default());
            let _ = BitBlt(self.mem, 0, 0, self.w as i32, self.h as i32, screen, self.rect.left, self.rect.top, SRCCOPY);
            ReleaseDC(HWND::default(), screen);
            draw_cursor(self.mem, self.rect.left, self.rect.top);
            std::slice::from_raw_parts(self.bits, (self.w * self.h * 4) as usize)
        }
    }
}

impl Drop for Grabber {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.mem, self.old);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.mem);
        }
    }
}

unsafe fn draw_cursor(dc: windows::Win32::Graphics::Gdi::HDC, ox: i32, oy: i32) {
    let mut ci = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
    if GetCursorInfo(&mut ci).is_err() || ci.flags.0 & CURSOR_SHOWING.0 == 0 || ci.hCursor.is_invalid() {
        return;
    }
    let mut info = ICONINFO::default();
    let mut hot = POINT::default();
    if GetIconInfo(ci.hCursor, &mut info).is_ok() {
        hot = POINT { x: info.xHotspot as i32, y: info.yHotspot as i32 };
        if !info.hbmColor.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
        }
        if !info.hbmMask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
        }
    }
    let _ = DrawIconEx(dc, ci.ptScreenPos.x - ox - hot.x, ci.ptScreenPos.y - oy - hot.y, ci.hCursor, 0, 0, 0, None, DI_NORMAL);
}
