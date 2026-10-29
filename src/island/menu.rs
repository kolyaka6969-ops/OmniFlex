//! Меню острова в его же стиле — скруглённая пилюля той же темы и прозрачности вместо меню Windows.
//! Работает как TrackPopupMenu: свой цикл сообщений, пока пункт не выбран или меню не закрыли
//! (Esc, щелчок мимо, переход в другое окно).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTONEAREST};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, PostQuitMessage, SetForegroundWindow, SetTimer,
    SetWindowDisplayAffinity, ShowWindow, TranslateMessage, MSG, SW_SHOWNOACTIVATE, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
    WM_ACTIVATE, WM_KEYDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_QUIT, WM_RBUTTONUP, WM_TIMER, WS_EX_LAYERED,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::view::{palette, Pal, P};
use crate::gfx::{self, Canvas, Weight};
use crate::wnd::{self, Handler};

const WM_MOUSELEAVE: u32 = 0x02A3;
const ROW_H: f32 = 32.0;
const SEP_H: f32 = 11.0;
const INSET: f32 = 6.0;
const RADIUS: f32 = 18.0;
const TEXT_X: f32 = 36.0;
const FADE: f32 = 0.12;

/// Пункт: номер (0 — разделитель), текст, отмечен, доступен.
pub(crate) type Item = (usize, String, bool, bool);

struct Menu {
    hwnd: HWND,
    items: Vec<Item>,
    /// Верх и высота каждого пункта в логических точках.
    rows: Vec<(f32, f32)>,
    w: f32,
    h: f32,
    scale: f32,
    pal: Pal,
    hover: Option<usize>,
    opened: Instant,
    alpha: f32,
    pos: (i32, i32),
    tracking: bool,
    result: Option<usize>,
}

thread_local! {
    static MENU: RefCell<Option<Rc<RefCell<Menu>>>> = const { RefCell::new(None) };
}

fn current() -> Option<Rc<RefCell<Menu>>> {
    MENU.with(|m| m.borrow().clone())
}

impl Menu {
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        let (x, y) = (x as f32 / self.scale, y as f32 / self.scale);
        if x < 0.0 || x > self.w {
            return None;
        }
        self.rows.iter().position(|&(top, h)| y >= top && y < top + h)
    }

    fn selectable(&self, i: usize) -> bool {
        self.items.get(i).map(|it| it.0 != 0 && it.3).unwrap_or(false)
    }

    /// Стрелки: следующий доступный пункт вверх или вниз, по кругу.
    fn step(&mut self, down: bool) {
        let n = self.items.len();
        if n == 0 {
            return;
        }
        let mut i = self.hover.unwrap_or(if down { n - 1 } else { 0 });
        for _ in 0..n {
            i = if down { (i + 1) % n } else { (i + n - 1) % n };
            if self.selectable(i) {
                self.hover = Some(i);
                return;
            }
        }
    }

    fn choose(&mut self, i: Option<usize>) {
        if let Some(i) = i.filter(|&i| self.selectable(i)) {
            self.result = Some(self.items[i].0);
        }
    }

    fn render(&self) {
        let pm = self.paint();
        let t = (self.opened.elapsed().as_secs_f32() / FADE).min(1.0);
        let alpha = (self.alpha * gfx::ease_out_cubic(t) * 255.0).round() as u8;
        wnd::update_layered_alpha(self.hwnd, Some(self.pos), &pm, alpha);
    }

    fn paint(&self) -> tiny_skia::Pixmap {
        let s = self.scale;
        let mut pm = gfx::pixmap((self.w * s).ceil() as u32, (self.h * s).ceil() as u32);
        {
            let pal = &self.pal;
            let mut p = P { c: Canvas::new(pm.as_mut()), s, ox: 0.0, oy: 0.0, hits: Vec::new() };
            p.rect(0.0, 0.0, self.w, self.h, RADIUS, pal.outline);
            p.rect(1.0, 1.0, self.w - 2.0, self.h - 2.0, RADIUS - 1.0, pal.bg);
            for (i, (id, text, checked, enabled)) in self.items.iter().enumerate() {
                let (top, h) = self.rows[i];
                if *id == 0 {
                    p.rect(INSET + 10.0, top + h / 2.0 - 0.5, self.w - 2.0 * (INSET + 10.0), 1.0, 0.5, pal.outline);
                    continue;
                }
                if self.hover == Some(i) && *enabled {
                    p.rect(INSET, top, self.w - 2.0 * INSET, h, RADIUS - INSET, pal.chip);
                }
                if *checked {
                    p.icon('\u{E73E}', 12.0, INSET + 15.0, top + h / 2.0, if *enabled { pal.accent } else { pal.faint });
                }
                let color = if *enabled { pal.fg } else { pal.faint };
                p.text_v(text, Weight::Semibold, 13.0, TEXT_X, top + h / 2.0, self.w - TEXT_X - 16.0, color);
            }
        }
        pm
    }
}

struct MenuHandler;

impl Handler for MenuHandler {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let menu = current()?;
        let mut m = menu.try_borrow_mut().ok()?;
        match msg {
            WM_MOUSEMOVE => {
                if !m.tracking {
                    let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                    unsafe {
                        let _ = TrackMouseEvent(&mut tme);
                    }
                    m.tracking = true;
                }
                let (x, y) = wnd::point_from_lparam(lparam);
                let hover = m.row_at(x, y).filter(|&i| m.selectable(i));
                if hover != m.hover {
                    m.hover = hover;
                    m.render();
                }
            }
            WM_MOUSELEAVE => {
                m.tracking = false;
                if m.hover.is_some() {
                    m.hover = None;
                    m.render();
                }
            }
            WM_LBUTTONUP | WM_RBUTTONUP => {
                let (x, y) = wnd::point_from_lparam(lparam);
                let row = m.row_at(x, y);
                m.choose(row);
            }
            WM_KEYDOWN => match wparam.0 {
                0x1B => m.result = Some(0),
                0x26 => {
                    m.step(false);
                    m.render();
                }
                0x28 => {
                    m.step(true);
                    m.render();
                }
                0x0D | 0x20 => {
                    let hover = m.hover;
                    m.choose(hover);
                }
                _ => {}
            },
            WM_ACTIVATE => {
                if wparam.0 & 0xFFFF == 0 && m.result.is_none() {
                    m.result = Some(0);
                }
                return None;
            }
            WM_TIMER => {
                if m.opened.elapsed().as_secs_f32() < FADE + 0.05 {
                    m.render();
                }
                // Щелчок мимо меню закрывает его, даже если окно так и не стало активным
                let pressed = unsafe { [0x01, 0x02, 0x04].iter().any(|&vk| GetAsyncKeyState(vk) as u16 & 0x8000 != 0) };
                if pressed {
                    let mut pt = POINT::default();
                    unsafe {
                        let _ = GetCursorPos(&mut pt);
                    }
                    let (x, y) = (pt.x - m.pos.0, pt.y - m.pos.1);
                    let (w, h) = ((m.w * m.scale) as i32, (m.h * m.scale) as i32);
                    if x < 0 || y < 0 || x > w || y > h {
                        m.result = Some(0);
                    }
                }
            }
            _ => return None,
        }
        Some(LRESULT(0))
    }
}

/// Пункты сверху вниз (верх и высота каждого), ширина и высота меню в логических точках.
fn layout(items: &[Item]) -> (Vec<(f32, f32)>, f32, f32) {
    let mut rows = Vec::with_capacity(items.len());
    let mut y = INSET;
    let mut text_w: f32 = 0.0;
    for (id, text, _, _) in items {
        let h = if *id == 0 { SEP_H } else { ROW_H };
        rows.push((y, h));
        y += h;
        if *id != 0 {
            text_w = text_w.max(gfx::text::measure(text, Weight::Semibold, 13.0));
        }
    }
    (rows, (TEXT_X + text_w + 22.0).clamp(190.0, 440.0), y + INSET)
}

/// Кадр меню для `--render-test`: второй пункт под курсором.
pub(crate) fn render_test(items: &[Item], scale: f32) -> tiny_skia::Pixmap {
    let (rows, w, h) = layout(items);
    let island = crate::settings::read(|s| s.island.clone());
    let menu = Menu {
        hwnd: HWND::default(),
        items: items.to_vec(),
        rows,
        w,
        h,
        scale,
        pal: palette(island.theme),
        hover: Some(1),
        opened: Instant::now(),
        alpha: 1.0,
        pos: (0, 0),
        tracking: false,
        result: None,
    };
    menu.paint()
}

/// Меню у курсора; возвращает номер выбранного пункта (0 — ничего).
pub(crate) fn popup(items: &[Item], scale: f32) -> usize {
    // Меню из меню не открываем
    if current().is_some() || items.is_empty() {
        return 0;
    }
    let island = crate::settings::read(|s| s.island.clone());
    let pal = palette(island.theme);
    let (rows, w, h) = layout(items);

    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    let monitor = unsafe { crate::win::monitor_info(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST)) }.unwrap_or_else(crate::win::primary_monitor);
    let (pw, ph) = ((w * scale).ceil() as i32, (h * scale).ceil() as i32);
    let work = monitor.work;
    let mut x = pt.x + 2;
    let mut top = pt.y + 6;
    if x + pw > work.right {
        x = (pt.x - pw).max(work.left);
    }
    if top + ph > work.bottom {
        top = (pt.y - ph - 2).max(work.top);
    }

    let Some(hwnd) = wnd::create(
        "OmniFlexMenu",
        "OmniFlex Menu",
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
        WS_POPUP,
        None,
        (x, top, pw, ph),
        Rc::new(RefCell::new(MenuHandler)),
    ) else {
        return 0;
    };
    let menu = Rc::new(RefCell::new(Menu {
        hwnd,
        items: items.to_vec(),
        rows,
        w,
        h,
        scale,
        pal,
        hover: None,
        opened: Instant::now(),
        alpha: island.opacity_expanded.clamp(20, 100) as f32 / 100.0,
        pos: (x, top),
        tracking: false,
        result: None,
    }));
    MENU.with(|m| *m.borrow_mut() = Some(menu.clone()));
    menu.borrow().render();
    unsafe {
        let affinity = if island.show_in_capture { WDA_NONE } else { WDA_EXCLUDEFROMCAPTURE };
        let _ = SetWindowDisplayAffinity(hwnd, affinity);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        // Активное меню получает клавиши и узнаёт о щелчке мимо
        let _ = SetForegroundWindow(hwnd);
        SetTimer(hwnd, 1, 16, None);
    }

    let mut result = 0;
    unsafe {
        let mut msg = MSG::default();
        loop {
            if let Some(r) = menu.borrow().result {
                result = r;
                break;
            }
            if !GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
                // Программа закрывается: вернуть WM_QUIT основному циклу
                if msg.message == WM_QUIT {
                    PostQuitMessage(msg.wParam.0 as i32);
                }
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        MENU.with(|m| *m.borrow_mut() = None);
        let _ = DestroyWindow(hwnd);
    }
    result
}
