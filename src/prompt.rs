//! Маленькие окна ввода под островом: вопрос ИИ, новая ссылка лаунчера, имя ярлыка. И окно для длинного
//! текста (ответ ИИ целиком). Остров фокус не забирает, поэтому печатать в нём нельзя — для этого эти окна.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, SelectObject, SetBkColor,
    SetBkMode, SetTextColor, DT_LEFT, DT_SINGLELINE, DT_VCENTER, HBRUSH, HDC, HFONT, HGDIOBJ, PAINTSTRUCT, TRANSPARENT,
};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, CreateWindowExW, DestroyWindow, GetClientRect, GetWindowLongPtrW, GetWindowTextLengthW,
    GetWindowTextW, PostMessageW, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, ShowWindow, GWLP_WNDPROC,
    SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_APP, WM_CHAR, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC,
    WM_KEYDOWN, WM_PAINT, WM_SETFONT, WNDPROC, WS_CHILD, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
    WS_VSCROLL,
};

use crate::util::wide;
use crate::wnd::{self, Handler};

const WM_SUBMIT: u32 = WM_APP + 41;
const WM_CANCEL: u32 = WM_APP + 42;
const ES_AUTOHSCROLL: u32 = 0x80;
const ES_MULTILINE: u32 = 0x4;
const ES_READONLY: u32 = 0x800;
const ES_AUTOVSCROLL: u32 = 0x40;
const EM_SETCUEBANNER: u32 = 0x1501;
const EM_SETSEL: u32 = 0xB1;

static EDIT_PROC: AtomicIsize = AtomicIsize::new(0);

struct Prompt {
    hwnd: HWND,
    edit: HWND,
    title: String,
    font: HFONT,
    title_font: HFONT,
    brush: HBRUSH,
    edit_brush: HBRUSH,
    callback: Option<Box<dyn FnOnce(String)>>,
    shown: bool,
    colors: Colors,
}

/// Цвета окна — как у острова: его тема, чёрный или светлый фон.
#[derive(Clone, Copy)]
struct Colors {
    bg: COLORREF,
    field: COLORREF,
    text: COLORREF,
    title: COLORREF,
    light: bool,
}

fn colors() -> Colors {
    let light = match crate::settings::read(|s| s.island.theme) {
        crate::settings::ThemeMode::Light => true,
        crate::settings::ThemeMode::Dark => false,
        crate::settings::ThemeMode::System => crate::win::system_light_theme(),
    };
    if light {
        Colors { bg: rgb(0xF5, 0xF5, 0xF7), field: rgb(0xE6, 0xE6, 0xE9), text: rgb(0x11, 0x11, 0x13), title: rgb(0x6E, 0x6E, 0x73), light }
    } else {
        Colors { bg: rgb(0, 0, 0), field: rgb(0x1F, 0x1F, 0x1F), text: rgb(0xF2, 0xF2, 0xF2), title: rgb(0xA8, 0xA8, 0xAD), light }
    }
}

thread_local! {
    static PROMPT: RefCell<Option<Rc<RefCell<Prompt>>>> = const { RefCell::new(None) };
}

fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

unsafe extern "system" fn edit_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let parent = windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd).unwrap_or_default();
    match msg {
        WM_KEYDOWN if wparam.0 == 0x0D => {
            let multiline = GetWindowLongPtrW(hwnd, windows::Win32::UI::WindowsAndMessaging::GWL_STYLE) as u32 & ES_MULTILINE != 0;
            if !multiline {
                let _ = PostMessageW(parent, WM_SUBMIT, WPARAM(0), LPARAM(0));
                return LRESULT(0);
            }
        }
        WM_KEYDOWN if wparam.0 == 0x1B => {
            let _ = PostMessageW(parent, WM_CANCEL, WPARAM(0), LPARAM(0));
            return LRESULT(0);
        }
        // Ctrl+A в многострочном поле Windows сам не делает
        WM_KEYDOWN if wparam.0 == 0x41 && windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x11) < 0 => {
            SendMessageW(hwnd, EM_SETSEL, WPARAM(0), LPARAM(-1));
            return LRESULT(0);
        }
        WM_CHAR if wparam.0 == 0x0D || wparam.0 == 0x1B || wparam.0 == 0x01 => return LRESULT(0),
        _ => {}
    }
    let orig: WNDPROC = std::mem::transmute(EDIT_PROC.load(Ordering::SeqCst));
    CallWindowProcW(orig, hwnd, msg, wparam, lparam)
}

struct PromptHandler;

impl Handler for PromptHandler {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let prompt = PROMPT.with(|p| p.borrow().clone())?;
        match msg {
            WM_PAINT => {
                unsafe {
                    let mut ps = PAINTSTRUCT::default();
                    let hdc = BeginPaint(hwnd, &mut ps);
                    if let Ok(p) = prompt.try_borrow() {
                        let mut rc = RECT::default();
                        let _ = GetClientRect(hwnd, &mut rc);
                        FillRect(hdc, &rc, p.brush);
                        let old = SelectObject(hdc, HGDIOBJ(p.title_font.0));
                        SetBkMode(hdc, TRANSPARENT);
                        SetTextColor(hdc, p.colors.title);
                        let scale = crate::win::monitor_scale(&crate::win::monitor_of_window(hwnd));
                        let mut title_rc = RECT { left: (16.0 * scale) as i32, top: 0, right: rc.right - 16, bottom: (34.0 * scale) as i32 };
                        let mut title = p.title.encode_utf16().collect::<Vec<u16>>();
                        DrawTextW(hdc, &mut title, &mut title_rc, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
                        SelectObject(hdc, old);
                    }
                    let _ = EndPaint(hwnd, &ps);
                }
                Some(LRESULT(0))
            }
            WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
                let p = prompt.try_borrow().ok()?;
                unsafe {
                    let hdc = HDC(wparam.0 as _);
                    SetTextColor(hdc, p.colors.text);
                    SetBkColor(hdc, p.colors.field);
                }
                Some(LRESULT(p.edit_brush.0 as isize))
            }
            WM_SUBMIT => {
                let (edit, callback) = {
                    let mut p = prompt.borrow_mut();
                    (p.edit, p.callback.take())
                };
                let text = unsafe {
                    let len = GetWindowTextLengthW(edit).max(0) as usize;
                    let mut buf = vec![0u16; len + 1];
                    GetWindowTextW(edit, &mut buf);
                    crate::util::from_wide(&buf)
                };
                close();
                if let Some(cb) = callback {
                    if !text.trim().is_empty() {
                        cb(text.trim().to_string());
                    }
                }
                Some(LRESULT(0))
            }
            WM_CANCEL => {
                close();
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                let active = wparam.0 & 0xFFFF != 0;
                let shown = prompt.try_borrow().map(|p| p.shown).unwrap_or(false);
                if !active && shown {
                    unsafe {
                        let _ = PostMessageW(hwnd, WM_CANCEL, WPARAM(0), LPARAM(0));
                    }
                }
                let _ = lparam;
                None
            }
            _ => None,
        }
    }
}

fn close() {
    let prompt = PROMPT.with(|p| p.borrow_mut().take());
    if let Some(p) = prompt {
        let p = p.borrow();
        unsafe {
            let _ = DestroyWindow(p.hwnd);
            let _ = DeleteObject(HGDIOBJ(p.font.0));
            let _ = DeleteObject(HGDIOBJ(p.title_font.0));
            let _ = DeleteObject(HGDIOBJ(p.brush.0));
            let _ = DeleteObject(HGDIOBJ(p.edit_brush.0));
        }
    }
}

fn font(size: f32, weight: i32) -> HFONT {
    let face = wide("Segoe UI");
    unsafe { CreateFontW(-(size.round() as i32), 0, 0, 0, weight, 0, 0, 0, 1, 0, 0, 5, 0, PCWSTR(face.as_ptr())) }
}

fn open(title: &str, placeholder: &str, initial: &str, multiline: bool, callback: Option<Box<dyn FnOnce(String)>>) {
    close();
    let monitor = crate::win::monitor_by_device(&crate::settings::read(|s| s.island.screen.clone()));
    let scale = crate::win::monitor_scale(&monitor);
    let (w, h) = if multiline { (640.0 * scale, 460.0 * scale) } else { (480.0 * scale, 92.0 * scale) };
    let x = monitor.work.left + ((monitor.work.right - monitor.work.left) as f32 - w) as i32 / 2;
    let y = monitor.work.top + (90.0 * scale) as i32;
    let Some(hwnd) = wnd::create(
        "OmniFlexPrompt",
        title,
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        WS_POPUP,
        None,
        (x, y, w as i32, h as i32),
        Rc::new(RefCell::new(PromptHandler)),
    ) else {
        return;
    };
    unsafe {
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &pref as *const _ as *const _, 4);
        let class = wide("EDIT");
        let text = wide(&initial.replace('\n', "\r\n"));
        let mut style = WS_CHILD.0 | WS_VISIBLE.0;
        style |= if multiline { ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL | WS_VSCROLL.0 } else { ES_AUTOHSCROLL };
        let pad = 16.0 * scale;
        let top = 34.0 * scale;
        let edit = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR(text.as_ptr()),
            WINDOW_STYLE(style),
            pad as i32,
            top as i32,
            (w - pad * 2.0) as i32,
            (h - top - pad) as i32,
            hwnd,
            None,
            None,
            None,
        )
        .unwrap_or_default();
        let f = font(if multiline { 14.5 } else { 17.0 } * scale, 400);
        let tf = font(12.5 * scale, 600);
        SendMessageW(edit, WM_SETFONT, WPARAM(f.0 as usize), LPARAM(1));
        if !placeholder.is_empty() {
            let cue = wide(placeholder);
            SendMessageW(edit, EM_SETCUEBANNER, WPARAM(1), LPARAM(cue.as_ptr() as isize));
        }
        let old = SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as isize);
        EDIT_PROC.store(old, Ordering::SeqCst);
        let colors = colors();
        // Рамка окна и полоса прокрутки — под тему острова
        let border = if colors.light { rgb(0xD8, 0xD8, 0xDC) } else { rgb(0x2A, 0x2A, 0x2A) };
        let _ = DwmSetWindowAttribute(hwnd, windows::Win32::Graphics::Dwm::DWMWA_BORDER_COLOR, &border as *const _ as *const _, 4);
        if !colors.light {
            let _ = windows::Win32::UI::Controls::SetWindowTheme(edit, windows::core::w!("DarkMode_Explorer"), PCWSTR::null());
        }
        let prompt = Prompt {
            hwnd,
            edit,
            title: title.to_string(),
            font: f,
            title_font: tf,
            brush: CreateSolidBrush(colors.bg),
            edit_brush: CreateSolidBrush(colors.field),
            callback,
            shown: false,
            colors,
        };
        PROMPT.with(|p| *p.borrow_mut() = Some(Rc::new(RefCell::new(prompt))));
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(edit);
        if !multiline {
            SendMessageW(edit, EM_SETSEL, WPARAM(0), LPARAM(-1));
        }
        PROMPT.with(|p| {
            if let Some(p) = p.borrow().as_ref() {
                p.borrow_mut().shown = true;
            }
        });
    }
}

/// Строка ввода под островом. Enter — готово, Esc или щелчок мимо — отмена.
pub fn ask(title: &str, placeholder: &str, initial: &str, callback: impl FnOnce(String) + 'static) {
    open(title, placeholder, initial, false, Some(Box::new(callback)));
}

/// Длинный текст только для чтения (можно выделить и скопировать).
pub fn show_text(title: &str, text: &str) {
    open(title, "", text, true, None);
}
