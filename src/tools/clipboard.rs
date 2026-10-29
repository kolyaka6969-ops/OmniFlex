//! Буфер обмена: текст, картинка и вставка без форматирования.

use std::time::Duration;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP,
    VIRTUAL_KEY,
};

use super::capture::Shot;

const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;

/// Буфер занят другой программой — пробуем несколько раз.
fn open() -> bool {
    for _ in 0..10 {
        if unsafe { OpenClipboard(HWND::default()).is_ok() } {
            return true;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    false
}

unsafe fn put(format: u32, bytes: &[u8]) {
    let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) else { return };
    let ptr = GlobalLock(mem);
    if ptr.is_null() {
        return;
    }
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
    let _ = GlobalUnlock(mem);
    let _ = SetClipboardData(format, HANDLE(mem.0));
}

pub fn set_text(text: &str) -> bool {
    if !open() {
        return false;
    }
    unsafe {
        let _ = EmptyClipboard();
        let w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = std::slice::from_raw_parts(w.as_ptr() as *const u8, w.len() * 2);
        put(CF_UNICODETEXT, bytes);
        let _ = CloseClipboard();
    }
    true
}

pub fn get_text() -> Option<String> {
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() || !open() {
            return None;
        }
        let mut result = None;
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT) {
            let mem = HGLOBAL(h.0);
            let ptr = GlobalLock(mem) as *const u16;
            if !ptr.is_null() {
                let max = GlobalSize(mem) / 2;
                let slice = std::slice::from_raw_parts(ptr, max);
                result = Some(crate::util::from_wide(slice));
                let _ = GlobalUnlock(mem);
            }
        }
        let _ = CloseClipboard();
        result
    }
}

/// Картинка: CF_DIB для всех программ и PNG для тех, что его понимают.
pub fn set_image(shot: &Shot) -> bool {
    let png = shot.png();
    if !open() {
        return false;
    }
    unsafe {
        let _ = EmptyClipboard();
        let header = crate::wnd::bitmap_info(shot.w as i32, shot.h as i32).bmiHeader;
        let mut header = header;
        // Снизу вверх: так CF_DIB понимают все
        header.biHeight = shot.h as i32;
        header.biSizeImage = shot.w * shot.h * 4;
        let mut dib = Vec::with_capacity(40 + shot.bgra.len());
        dib.extend_from_slice(std::slice::from_raw_parts(&header as *const _ as *const u8, std::mem::size_of_val(&header)));
        for row in (0..shot.h).rev() {
            let start = (row * shot.w * 4) as usize;
            dib.extend_from_slice(&shot.bgra[start..start + (shot.w * 4) as usize]);
        }
        put(CF_DIB, &dib);
        let name = crate::util::wide("PNG");
        let png_format = RegisterClipboardFormatW(PCWSTR(name.as_ptr()));
        if png_format != 0 {
            put(png_format, &png);
        }
        let _ = CloseClipboard();
    }
    true
}

fn key(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Ждёт, пока отпустят модификаторы горячей клавиши, и жмёт Ctrl+V в активном окне.
pub fn send_paste() {
    std::thread::spawn(|| {
        for _ in 0..60 {
            let held = [0x10, 0x11, 0x12, 0x5B, 0x5C].iter().any(|&vk| unsafe { GetAsyncKeyState(vk) as u16 & 0x8000 != 0 });
            if !held {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let inputs = [key(0x11, false), key(0x56, false), key(0x56, true), key(0x11, true)];
        unsafe {
            SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        }
    });
}

/// Вставка без форматирования: в буфере остаётся только текст, и он вставляется в активное окно.
pub fn paste_plain() -> bool {
    let Some(text) = get_text() else { return false };
    if !set_text(&text) {
        return false;
    }
    send_paste();
    true
}
