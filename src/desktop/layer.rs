//! Окно в слое обоев: поверх картинки рабочего стола, но под значками — так же устроены Lively и Wallpaper Engine.
//! Окно дочернее для Explorer, поэтому создаётся и живёт только в потоке обоев.

use std::cell::RefCell;
use std::rc::Rc;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, ScreenToClient, StretchDIBits, DIB_RGB_COLORS, PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, EnumWindows, FindWindowExW, FindWindowW, GetAncestor, GetWindow, GetWindowLongPtrW,
    GetWindowRect, IsWindow, SendMessageTimeoutW, SetLayeredWindowAttributes, SetWindowPos, SystemParametersInfoW,
    GA_PARENT, GWL_EXSTYLE, GW_CHILD, GW_HWNDNEXT, HWND_TOP, LWA_ALPHA, SMTO_NORMAL, SPIF_UPDATEINIFILE,
    SPI_SETDESKWALLPAPER, SWP_NOACTIVATE, SWP_SHOWWINDOW, WM_ERASEBKGND, WM_PAINT, WS_CHILD, WS_CLIPCHILDREN,
    WS_CLIPSIBLINGS, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP,
};

use super::view::View;
use crate::util::wide;
use crate::wnd::{self, Handler};
use crate::win;

/// Куда вставлять окно. Windows 11 24H2+: значки лежат прямо в Progman, окно встаёт сразу под ними (raised).
/// Раньше: по сообщению 0x052C Progman создаёт WorkerW позади значков, окно встаёт в него поверх всего.
/// У родителя без своей поверхности (WS_EX_NOREDIRECTIONBITMAP) окно должно быть слоистым, иначе его не видно.
#[derive(Clone, Copy, Debug)]
struct Target {
    parent: HWND,
    icons: HWND,
    icons_host: HWND,
    raised: bool,
    layered: bool,
}

const WM_SPAWN_WORKER: u32 = 0x052C;

pub struct Layer {
    device: String,
    view: Rc<RefCell<View>>,
    hwnd: Option<HWND>,
    target: Option<Target>,
    visible: bool,
}

struct LayerWindow {
    view: Rc<RefCell<View>>,
}

impl Handler for LayerWindow {
    fn handle(&mut self, hwnd: HWND, msg: u32, _w: WPARAM, _l: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_PAINT => {
                unsafe {
                    let mut ps = PAINTSTRUCT::default();
                    let dc = BeginPaint(hwnd, &mut ps);
                    if let Ok(view) = self.view.try_borrow() {
                        let frame = view.frame();
                        let (w, h) = (frame.width() as i32, frame.height() as i32);
                        let r = ps.rcPaint;
                        let info = wnd::bitmap_info(w, h);
                        let x = r.left.clamp(0, w);
                        let y = r.top.clamp(0, h);
                        let cw = (r.right.min(w) - x).max(0);
                        let ch = (r.bottom.min(h) - y).max(0);
                        if cw > 0 && ch > 0 {
                            // Буфер сверху вниз: у такого DIB координаты источника считаются от верхнего края
                            StretchDIBits(
                                dc,
                                x,
                                y,
                                cw,
                                ch,
                                x,
                                y,
                                cw,
                                ch,
                                Some(frame.data().as_ptr() as *const _),
                                &info,
                                DIB_RGB_COLORS,
                                SRCCOPY,
                            );
                        }
                    }
                    let _ = EndPaint(hwnd, &ps);
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

impl Layer {
    pub fn new(device: String, view: Rc<RefCell<View>>) -> Self {
        Self { device, view, hwnd: None, target: None, visible: false }
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    pub fn hwnd(&self) -> Option<HWND> {
        self.hwnd
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    pub fn set_visible(&mut self, visible: bool) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.ensure();
        } else {
            self.destroy();
        }
    }

    /// Окно на месте: Explorer могут перезапустить, экран — сменить, другая программа обоев — перекрыть.
    pub fn ensure(&mut self) {
        if !self.visible {
            return;
        }
        unsafe {
            if let (Some(hwnd), Some(t)) = (self.hwnd, self.target) {
                // Значки могли переехать: Explorer при запуске сначала держит их в Progman и лишь потом переносит
                if IsWindow(hwnd).as_bool()
                    && IsWindow(t.icons).as_bool()
                    && GetAncestor(hwnd, GA_PARENT) == t.parent
                    && GetAncestor(t.icons, GA_PARENT) == t.icons_host
                {
                    self.place(hwnd, t, true);
                    return;
                }
            }
        }
        self.destroy();
        let Some(target) = find_target() else { return }; // Explorer ещё запускается
        self.target = Some(target);
        let (x, y, w, h) = bounds(target, &self.device);
        // Монитор мог смениться — подгоняем буфер вида
        if let Ok(mut v) = self.view.try_borrow_mut() {
            v.resize(w as u32, h as u32);
        }
        let ex = if target.layered { WS_EX_NOACTIVATE | WS_EX_LAYERED } else { WS_EX_NOACTIVATE };
        // Без WS_VISIBLE: новое дочернее окно встаёт поверх значков, показываем его уже под ними
        let hwnd = wnd::create(
            "OmniFlexDesktop",
            "OmniFlex desktop",
            ex,
            WS_CHILD | WS_CLIPSIBLINGS | WS_CLIPCHILDREN,
            Some(target.parent),
            (x, y, w, h),
            Rc::new(RefCell::new(LayerWindow { view: self.view.clone() })),
        );
        let Some(hwnd) = hwnd else {
            self.target = None;
            return;
        };
        if target.layered {
            unsafe {
                let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            }
        }
        self.hwnd = Some(hwnd);
        self.place(hwnd, target, false);
    }

    fn place(&self, hwnd: HWND, target: Target, only_if_moved: bool) {
        unsafe {
            if only_if_moved {
                let screen = win::monitor_by_device(&self.device).bounds;
                let mut r = RECT::default();
                let in_place = GetWindowRect(hwnd, &mut r).is_ok()
                    && r.left == screen.left
                    && r.top == screen.top
                    && r.right == screen.right
                    && r.bottom == screen.bottom;
                if in_place && in_order(hwnd, target) {
                    return;
                }
            }
            let (x, y, w, h) = bounds(target, &self.device);
            let after = if target.raised { target.icons } else { HWND_TOP };
            let _ = SetWindowPos(hwnd, after, x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }

    fn destroy(&mut self) {
        let raised = self.target.map(|t| t.raised).unwrap_or(true);
        if let Some(hwnd) = self.hwnd.take() {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            // В старой схеме WorkerW сам не перерисовывается и оставил бы последний кадр — заново ставим текущие обои.
            if !raised {
                if let Some(wallpaper) = win::wallpaper_path() {
                    let w = wide(&wallpaper);
                    unsafe {
                        let _ = SystemParametersInfoW(SPI_SETDESKWALLPAPER, 0, Some(w.as_ptr() as *mut _), SPIF_UPDATEINIFILE);
                    }
                }
            }
        }
        self.target = None;
    }
}

impl Drop for Layer {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// Весь монитор в координатах родителя (родитель охватывает все мониторы).
fn bounds(target: Target, device: &str) -> (i32, i32, i32, i32) {
    let screen = win::monitor_by_device(device).bounds;
    let mut origin = POINT { x: screen.left, y: screen.top };
    unsafe {
        let _ = ScreenToClient(target.parent, &mut origin);
    }
    (origin.x, origin.y, screen.right - screen.left, screen.bottom - screen.top)
}

/// Окно стоит выше всего остального в слое обоев (свои окна на других мониторах не считаются).
fn in_order(hwnd: HWND, target: Target) -> bool {
    unsafe {
        let mut w = if target.raised { GetWindow(target.icons, GW_HWNDNEXT) } else { GetWindow(target.parent, GW_CHILD) }
            .unwrap_or_default();
        while !w.0.is_null() && w != hwnd && win::window_pid(w) == win::own_pid() {
            w = GetWindow(w, GW_HWNDNEXT).unwrap_or_default();
        }
        w == hwnd
    }
}

fn no_surface(hwnd: HWND) -> bool {
    unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_NOREDIRECTIONBITMAP.0 != 0 }
}

fn find_target() -> Option<Target> {
    unsafe {
        let progman = FindWindowW(PCWSTR(wide("Progman").as_ptr()), PCWSTR::null()).ok()?;
        if progman.0.is_null() {
            return None;
        }
        // Параметры (0xD, 1) — как у Wallpaper Engine и Lively; часть сборок Windows 10 понимает только (0, 0)
        let _ = SendMessageTimeoutW(progman, WM_SPAWN_WORKER, WPARAM(0xD), LPARAM(1), SMTO_NORMAL, 1000, None);

        if no_surface(progman) {
            let icons = FindWindowExW(progman, HWND::default(), PCWSTR(wide("SHELLDLL_DefView").as_ptr()), PCWSTR::null())
                .unwrap_or_default();
            if !icons.0.is_null() {
                return Some(Target { parent: progman, icons, icons_host: progman, raised: true, layered: true });
            }
        }

        let mut found = find_worker();
        if found.2.0.is_null() && !no_surface(found.1) {
            let _ = SendMessageTimeoutW(progman, WM_SPAWN_WORKER, WPARAM(0), LPARAM(0), SMTO_NORMAL, 1000, None);
            found = find_worker();
        }
        let (icons, host, worker) = found;
        if icons.0.is_null() {
            return None;
        }
        if !worker.0.is_null() {
            return Some(Target { parent: worker, icons, icons_host: host, raised: false, layered: no_surface(worker) });
        }
        // Слоя позади значков нет. Встать под значки в их окне можно, только если оно без своей поверхности
        if no_surface(host) {
            Some(Target { parent: host, icons, icons_host: host, raised: true, layered: true })
        } else {
            None
        }
    }
}

/// Значки, окно с ними и слой WorkerW позади этого окна (если есть).
fn find_worker() -> (HWND, HWND, HWND) {
    struct Found(HWND, HWND, HWND);
    unsafe extern "system" fn callback(top: HWND, data: LPARAM) -> BOOL {
        let found = &mut *(data.0 as *mut Found);
        let view = FindWindowExW(top, HWND::default(), PCWSTR(wide("SHELLDLL_DefView").as_ptr()), PCWSTR::null())
            .unwrap_or_default();
        if view.0.is_null() {
            return true.into();
        }
        found.0 = view;
        found.1 = top;
        // Первый WorkerW ниже окна со значками в Z-порядке
        found.2 = FindWindowExW(HWND::default(), top, PCWSTR(wide("WorkerW").as_ptr()), PCWSTR::null()).unwrap_or_default();
        false.into()
    }
    let mut found = Found(HWND::default(), HWND::default(), HWND::default());
    unsafe {
        let _ = EnumWindows(Some(callback), LPARAM(&mut found as *mut _ as isize));
    }
    (found.0, found.1, found.2)
}
