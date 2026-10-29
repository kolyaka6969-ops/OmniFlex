//! Свои окна на WinAPI: регистрация классов, обработчики сообщений, очередь задач потока
//! и обновление слоистых окон из буфера tiny-skia.

use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::rc::Rc;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject, AC_SRC_ALPHA,
    AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, GetWindowLongPtrW, LoadCursorW, PostMessageW,
    RegisterClassExW, SetWindowLongPtrW, TranslateMessage, UpdateLayeredWindow, CREATESTRUCTW, CS_HREDRAW,
    CS_VREDRAW, GWLP_USERDATA, HICON, HMENU, HWND_MESSAGE, IDC_ARROW, MSG, ULW_ALPHA, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_APP, WM_NCCREATE, WM_NCDESTROY, WNDCLASSEXW,
};

use crate::util::wide;

/// Обработчик сообщений своего окна. None — пусть отработает DefWindowProc.
pub trait Handler {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT>;
}

type Shared = Rc<RefCell<dyn Handler>>;

thread_local! {
    static CLASSES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = &*(lparam.0 as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Shared;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    if msg == WM_NCDESTROY {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let handler = Box::from_raw(ptr);
        if let Ok(mut h) = handler.try_borrow_mut() {
            h.handle(hwnd, msg, wparam, lparam);
        }
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let handler = (*ptr).clone();
    // Сообщение пришло, пока обработчик уже работает (SetWindowPos и подобное шлют их сразу) — отдаём системе
    let result = match handler.try_borrow_mut() {
        Ok(mut h) => {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h.handle(hwnd, msg, wparam, lparam)));
            match r {
                Ok(r) => r,
                Err(_) => {
                    crate::util::log_error(format!("panic в обработчике окна, сообщение {msg:#x}"));
                    None
                }
            }
        }
        Err(_) => None,
    };
    match result {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn register(class: &str, icon: Option<HICON>) {
    CLASSES.with(|classes| {
        if classes.borrow().contains(class) {
            return;
        }
        let name = wide(class);
        unsafe {
            let instance = GetModuleHandleW(None).unwrap_or_default();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wndproc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hIcon: icon.unwrap_or_default(),
                lpszClassName: PCWSTR(name.as_ptr()),
                ..Default::default()
            };
            RegisterClassExW(&wc);
        }
        classes.borrow_mut().insert(class.to_string());
    });
}

/// Создаёт окно своего класса с обработчиком. Класс регистрируется при первом окне.
pub fn create(
    class: &str,
    title: &str,
    ex_style: WINDOW_EX_STYLE,
    style: WINDOW_STYLE,
    parent: Option<HWND>,
    rect: (i32, i32, i32, i32),
    handler: Shared,
) -> Option<HWND> {
    register(class, None);
    let class_w = wide(class);
    let title_w = wide(title);
    let boxed: *mut Shared = Box::into_raw(Box::new(handler));
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let hwnd = CreateWindowExW(
            ex_style,
            PCWSTR(class_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            style,
            rect.0,
            rect.1,
            rect.2,
            rect.3,
            parent.unwrap_or_default(),
            HMENU::default(),
            instance,
            Some(boxed as *const _),
        );
        match hwnd {
            Ok(h) if !h.0.is_null() => Some(h),
            _ => {
                // Окно не создалось — WM_NCDESTROY не придёт, обработчик освобождаем сами
                drop(Box::from_raw(boxed));
                None
            }
        }
    }
}

/// Окно только для сообщений (горячие клавиши, трей, очередь задач).
pub fn create_message_window(class: &str, handler: Shared) -> Option<HWND> {
    create(class, class, WINDOW_EX_STYLE(0), WINDOW_STYLE(0), Some(HWND_MESSAGE), (0, 0, 0, 0), handler)
}

pub fn run_message_loop() {
    unsafe {
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

// ---------- Очередь задач потока ----------

pub const WM_RUN_QUEUE: u32 = WM_APP + 0x100;

type Task = Box<dyn FnOnce() + Send>;

/// Очередь задач для потока со своим циклом сообщений: задачи из других потоков выполняются в нём.
/// Сообщение шлётся окну, а не потоку: сообщения потоку теряются в модальных циклах (меню, перетаскивание).
pub struct Dispatcher {
    hwnd: AtomicIsize,
    queue: Mutex<VecDeque<Task>>,
}

struct DispatcherHandler(&'static Dispatcher);

impl Handler for DispatcherHandler {
    fn handle(&mut self, _hwnd: HWND, msg: u32, _w: WPARAM, _l: LPARAM) -> Option<LRESULT> {
        if msg == WM_RUN_QUEUE {
            self.0.drain();
            return Some(LRESULT(0));
        }
        None
    }
}

impl Dispatcher {
    pub const fn new() -> Self {
        Self { hwnd: AtomicIsize::new(0), queue: Mutex::new(VecDeque::new()) }
    }

    /// Привязывает очередь к текущему потоку. Вызывать один раз, из потока с циклом сообщений.
    pub fn attach(&'static self, class: &str) {
        if let Some(hwnd) = create_message_window(class, Rc::new(RefCell::new(DispatcherHandler(self)))) {
            self.hwnd.store(hwnd.0 as isize, Ordering::SeqCst);
            self.wake();
        }
    }

    pub fn post(&self, task: impl FnOnce() + Send + 'static) {
        if let Ok(mut q) = self.queue.lock() {
            q.push_back(Box::new(task));
        }
        self.wake();
    }

    pub fn is_attached(&self) -> bool {
        self.hwnd.load(Ordering::SeqCst) != 0
    }

    fn wake(&self) {
        let hwnd = self.hwnd.load(Ordering::SeqCst);
        if hwnd != 0 {
            unsafe {
                let _ = PostMessageW(HWND(hwnd as _), WM_RUN_QUEUE, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn drain(&self) {
        loop {
            let task = match self.queue.lock() {
                Ok(mut q) => q.pop_front(),
                Err(_) => None,
            };
            match task {
                Some(task) => {
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(task)).is_err() {
                        crate::util::log_error("panic в задаче потока окон");
                    }
                }
                None => break,
            }
        }
    }
}

// ---------- Слоистые окна ----------

/// Кадр в слоистое окно с попиксельной прозрачностью. Буфер — BGRA с премультипликацией.
/// pos — левый верхний угол на экране; None — окно остаётся где было (дочерние окна).
pub fn update_layered(hwnd: HWND, pos: Option<(i32, i32)>, pm: &tiny_skia::Pixmap) {
    update_layered_alpha(hwnd, pos, pm, 255);
}

/// То же с общей прозрачностью окна (0–255).
pub fn update_layered_alpha(hwnd: HWND, pos: Option<(i32, i32)>, pm: &tiny_skia::Pixmap, alpha: u8) {
    let w = pm.width() as i32;
    let h = pm.height() as i32;
    unsafe {
        let screen = GetDC(HWND::default());
        let mem = CreateCompatibleDC(screen);
        if let Some(bitmap) = dib_from_pixmap(screen, pm) {
            let old = SelectObject(mem, HGDIOBJ(bitmap.0));
            let size = SIZE { cx: w, cy: h };
            let src = POINT { x: 0, y: 0 };
            let dst = pos.map(|(x, y)| POINT { x, y });
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: alpha,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let _ = UpdateLayeredWindow(
                hwnd,
                screen,
                dst.as_ref().map(|p| p as *const POINT),
                Some(&size),
                mem,
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            SelectObject(mem, old);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
        }
        let _ = DeleteDC(mem);
        ReleaseDC(HWND::default(), screen);
    }
}

pub fn bitmap_info(w: i32, h: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // сверху вниз
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

unsafe fn dib_from_pixmap(dc: HDC, pm: &tiny_skia::Pixmap) -> Option<HBITMAP> {
    let info = bitmap_info(pm.width() as i32, pm.height() as i32);
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
    if bits.is_null() {
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        return None;
    }
    let data = pm.data();
    std::ptr::copy_nonoverlapping(data.as_ptr(), bits as *mut u8, data.len());
    Some(bitmap)
}

/// Координаты мыши из LPARAM (со знаком: на втором мониторе слева они отрицательные).
pub fn point_from_lparam(lparam: LPARAM) -> (i32, i32) {
    let x = (lparam.0 & 0xFFFF) as u16 as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
    (x, y)
}
