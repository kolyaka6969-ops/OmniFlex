//! Затемнённый снимок экрана поверх всего: выделение области (скриншот, текст с экрана, QR, запись),
//! разметка скриншота (стрелки, рамки, маркер, текст, размытие личных данных) и пипетка с лупой.
//! Окно — одно на весь виртуальный рабочий стол, координаты в физических пикселях.

use std::cell::RefCell;
use std::rc::Rc;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, SetDIBitsToDevice, DIB_RGB_COLORS, PAINTSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus, VK_CONTROL};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, GetCursorPos, LoadCursorW, SetCursor, SetCursorPos, SetForegroundWindow, ShowWindow, IDC_ARROW,
    IDC_CROSS, SW_SHOW, WM_CHAR, WM_DESTROY, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
    WM_PAINT, WM_RBUTTONUP, WM_SETCURSOR, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::capture::{self, Shot};
use crate::gfx::{Canvas, Rgba, Weight};
use crate::i18n::tr;
use crate::wnd::{self, Handler};
use crate::win;

pub enum Purpose {
    Screenshot,
    /// Выделить область и отдать её (текст с экрана, QR, запись).
    Region(Box<dyn FnOnce(RECT, Shot)>),
    Picker,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Tool {
    Arrow,
    Rect,
    Marker,
    Text,
    Blur,
}

#[derive(Clone, Debug)]
enum Mark {
    Arrow((f32, f32), (f32, f32), Rgba),
    Rect((f32, f32), (f32, f32), Rgba),
    Marker(Vec<(f32, f32)>, Rgba),
    Text((f32, f32), String, Rgba),
    Blur((f32, f32), (f32, f32)),
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Button {
    Tool(Tool),
    Color,
    Undo,
    Done,
    Close,
}

const COLORS: [Rgba; 6] = [
    Rgba::rgb(0xFF, 0x3B, 0x30),
    Rgba::rgb(0xFF, 0xCC, 0x00),
    Rgba::rgb(0x34, 0xC7, 0x59),
    Rgba::rgb(0x0A, 0x84, 0xFF),
    Rgba::rgb(0xFF, 0xFF, 0xFF),
    Rgba::rgb(0x11, 0x11, 0x11),
];

const BUTTONS: [Button; 9] = [
    Button::Tool(Tool::Arrow),
    Button::Tool(Tool::Rect),
    Button::Tool(Tool::Marker),
    Button::Tool(Tool::Text),
    Button::Tool(Tool::Blur),
    Button::Color,
    Button::Undo,
    Button::Done,
    Button::Close,
];

struct Overlay {
    hwnd: HWND,
    purpose: Option<Purpose>,
    shot: Shot,
    dim: Vec<u8>,
    frame: Vec<u8>,
    drag: Option<(f32, f32)>,
    sel: Option<(f32, f32, f32, f32)>,
    editing: bool,
    tool: Tool,
    color: usize,
    marks: Vec<Mark>,
    current: Option<Mark>,
    text: Option<((f32, f32), String)>,
    cursor: (f32, f32),
    hover: Option<Button>,
    buttons: Vec<(f32, f32, f32, f32, Button)>,
    scale: f32,
}

thread_local! {
    static OVERLAY: RefCell<Option<Rc<RefCell<Overlay>>>> = const { RefCell::new(None) };
}

pub fn is_open() -> bool {
    OVERLAY.with(|o| o.borrow().is_some())
}

/// Открывает затемнённый снимок экрана. Вызывать в потоке окон.
pub fn start(purpose: Purpose) {
    if is_open() {
        return;
    }
    let vs = capture::virtual_screen();
    let Some(shot) = capture::grab(vs, false) else { return };
    let dim: Vec<u8> = shot.bgra.iter().enumerate().map(|(i, &v)| if i % 4 == 3 { 255 } else { (v as u32 * 115 / 255) as u8 }).collect();
    let frame = shot.bgra.clone();
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    let scale = win::monitor_scale(&win::monitor_at(pt));
    let overlay = Rc::new(RefCell::new(Overlay {
        hwnd: HWND::default(),
        purpose: Some(purpose),
        cursor: ((pt.x - shot.x) as f32, (pt.y - shot.y) as f32),
        shot,
        dim,
        frame,
        drag: None,
        sel: None,
        editing: false,
        tool: Tool::Arrow,
        color: 0,
        marks: Vec::new(),
        current: None,
        text: None,
        hover: None,
        buttons: Vec::new(),
        scale,
    }));
    let Some(hwnd) = wnd::create(
        "OmniFlexOverlay",
        "OmniFlex",
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
        WS_POPUP,
        None,
        (vs.left, vs.top, vs.right - vs.left, vs.bottom - vs.top),
        Rc::new(RefCell::new(OverlayHandler)),
    ) else {
        return;
    };
    overlay.borrow_mut().hwnd = hwnd;
    OVERLAY.with(|o| *o.borrow_mut() = Some(overlay));
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
    }
}

fn close() {
    let overlay = OVERLAY.with(|o| o.borrow_mut().take());
    if let Some(o) = overlay {
        let hwnd = o.borrow().hwnd;
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

struct OverlayHandler;

impl Handler for OverlayHandler {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let overlay = OVERLAY.with(|o| o.borrow().clone());
        let Some(overlay) = overlay else { return None };
        match msg {
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_PAINT => {
                unsafe {
                    let mut ps = PAINTSTRUCT::default();
                    let hdc = BeginPaint(hwnd, &mut ps);
                    if let Ok(mut o) = overlay.try_borrow_mut() {
                        o.render();
                        let info = wnd::bitmap_info(o.shot.w as i32, o.shot.h as i32);
                        SetDIBitsToDevice(hdc, 0, 0, o.shot.w, o.shot.h, 0, 0, 0, o.shot.h, o.frame.as_ptr() as *const _, &info, DIB_RGB_COLORS);
                    }
                    let _ = EndPaint(hwnd, &ps);
                }
                Some(LRESULT(0))
            }
            WM_SETCURSOR => {
                let over_button = overlay.try_borrow().map(|o| o.hover.is_some()).unwrap_or(false);
                unsafe {
                    let _ = SetCursor(LoadCursorW(None, if over_button { IDC_ARROW } else { IDC_CROSS }).unwrap_or_default());
                }
                Some(LRESULT(1))
            }
            WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONUP => {
                let (x, y) = wnd::point_from_lparam(lparam);
                let action = overlay.try_borrow_mut().ok().and_then(|mut o| o.mouse(msg, x as f32, y as f32));
                self.after(hwnd, action);
                Some(LRESULT(0))
            }
            WM_KEYDOWN => {
                let action = overlay.try_borrow_mut().ok().and_then(|mut o| o.key(wparam.0 as u32));
                self.after(hwnd, action);
                Some(LRESULT(0))
            }
            WM_CHAR => {
                if let Ok(mut o) = overlay.try_borrow_mut() {
                    o.char(wparam.0 as u32);
                }
                unsafe {
                    let _ = InvalidateRect(hwnd, None, false);
                }
                Some(LRESULT(0))
            }
            WM_DESTROY => None,
            _ => None,
        }
    }
}

enum Action {
    Close,
    Finish,
}

impl OverlayHandler {
    fn after(&self, hwnd: HWND, action: Option<Action>) {
        match action {
            Some(Action::Close) => close(),
            Some(Action::Finish) => finish(),
            None => unsafe {
                let _ = InvalidateRect(hwnd, None, false);
            },
        }
    }
}

/// Готово: отдать результат и закрыть окно.
fn finish() {
    let overlay = OVERLAY.with(|o| o.borrow_mut().take());
    let Some(o) = overlay else { return };
    let (hwnd, purpose, result, rect, picked) = {
        let mut o = o.borrow_mut();
        o.commit_text();
        let purpose = o.purpose.take();
        let rect = o.sel_rect();
        let result = rect.map(|r| o.export(r));
        let picked = o.shot.pixel(o.shot.x + o.cursor.0 as i32, o.shot.y + o.cursor.1 as i32);
        (o.hwnd, purpose, result, rect, picked)
    };
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    match purpose {
        Some(Purpose::Screenshot) => {
            if let Some(shot) = result {
                super::screenshot_taken(shot);
            }
        }
        Some(Purpose::Region(callback)) => {
            if let (Some(r), Some(shot)) = (rect, result) {
                callback(r, shot);
            }
        }
        Some(Purpose::Picker) => {
            if let Some(rgb) = picked {
                super::color_picked(rgb);
            }
        }
        None => {}
    }
}

fn norm(a: (f32, f32), b: (f32, f32)) -> (f32, f32, f32, f32) {
    (a.0.min(b.0), a.1.min(b.1), (a.0 - b.0).abs(), (a.1 - b.1).abs())
}

impl Overlay {
    fn is_picker(&self) -> bool {
        matches!(self.purpose, Some(Purpose::Picker))
    }

    fn is_screenshot(&self) -> bool {
        matches!(self.purpose, Some(Purpose::Screenshot))
    }

    /// Выделение в координатах экрана.
    fn sel_rect(&self) -> Option<RECT> {
        let (x, y, w, h) = self.sel?;
        let left = self.shot.x + x.round() as i32;
        let top = self.shot.y + y.round() as i32;
        Some(RECT { left, top, right: left + (w.round() as i32).max(1), bottom: top + (h.round() as i32).max(1) })
    }

    fn mouse(&mut self, msg: u32, x: f32, y: f32) -> Option<Action> {
        self.cursor = (x, y);
        let button = self.buttons.iter().find(|b| x >= b.0 && x < b.0 + b.2 && y >= b.1 && y < b.1 + b.3).map(|b| b.4);
        if msg == WM_MOUSEMOVE {
            self.hover = button;
        }
        if self.is_picker() {
            return match msg {
                WM_LBUTTONUP => Some(Action::Finish),
                WM_RBUTTONUP => Some(Action::Close),
                _ => None,
            };
        }
        match msg {
            WM_RBUTTONUP => return Some(Action::Close),
            WM_LBUTTONDOWN => {
                if let Some(b) = button {
                    return self.press(b);
                }
                if !self.editing {
                    self.drag = Some((x, y));
                    self.sel = Some((x, y, 0.0, 0.0));
                    return None;
                }
                let Some((sx, sy, sw, sh)) = self.sel else { return None };
                if x < sx || y < sy || x > sx + sw || y > sy + sh {
                    return None;
                }
                self.commit_text();
                let color = COLORS[self.color];
                self.current = match self.tool {
                    Tool::Arrow => Some(Mark::Arrow((x, y), (x, y), color)),
                    Tool::Rect => Some(Mark::Rect((x, y), (x, y), color)),
                    Tool::Marker => Some(Mark::Marker(vec![(x, y)], color)),
                    Tool::Blur => Some(Mark::Blur((x, y), (x, y))),
                    Tool::Text => {
                        self.text = Some(((x, y), String::new()));
                        None
                    }
                };
            }
            WM_MOUSEMOVE => {
                if let Some(start) = self.drag {
                    let (rx, ry, rw, rh) = norm(start, (x, y));
                    self.sel = Some((rx, ry, rw, rh));
                }
                let (sx, sy, sw, sh) = self.sel.unwrap_or((0.0, 0.0, 0.0, 0.0));
                let clamped = (x.clamp(sx, sx + sw), y.clamp(sy, sy + sh));
                match &mut self.current {
                    Some(Mark::Arrow(_, end, _)) | Some(Mark::Rect(_, end, _)) | Some(Mark::Blur(_, end)) => *end = clamped,
                    Some(Mark::Marker(points, _)) => points.push(clamped),
                    _ => {}
                }
            }
            WM_LBUTTONUP => {
                if let Some(start) = self.drag.take() {
                    let (rx, ry, rw, rh) = norm(start, (x, y));
                    if rw < 4.0 || rh < 4.0 {
                        // Щелчок без выделения — весь монитор под курсором
                        let m = win::monitor_at(POINT { x: self.shot.x + x as i32, y: self.shot.y + y as i32 });
                        let b = m.bounds;
                        self.sel = Some(((b.left - self.shot.x) as f32, (b.top - self.shot.y) as f32, (b.right - b.left) as f32, (b.bottom - b.top) as f32));
                    } else {
                        self.sel = Some((rx, ry, rw, rh));
                    }
                    if !self.is_screenshot() {
                        return Some(Action::Finish);
                    }
                    self.editing = true;
                    self.layout_toolbar();
                    return None;
                }
                if let Some(mark) = self.current.take() {
                    let keep = match &mark {
                        Mark::Arrow(a, b, _) | Mark::Rect(a, b, _) | Mark::Blur(a, b) => (a.0 - b.0).abs() + (a.1 - b.1).abs() > 3.0,
                        _ => true,
                    };
                    if keep {
                        self.marks.push(mark);
                    }
                }
            }
            _ => {}
        }
        None
    }

    fn press(&mut self, b: Button) -> Option<Action> {
        match b {
            Button::Tool(t) => {
                self.commit_text();
                self.tool = t;
            }
            Button::Color => self.color = (self.color + 1) % COLORS.len(),
            Button::Undo => {
                if self.text.take().is_none() {
                    self.marks.pop();
                }
            }
            Button::Done => return Some(Action::Finish),
            Button::Close => return Some(Action::Close),
        }
        None
    }

    fn key(&mut self, vk: u32) -> Option<Action> {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        match vk {
            0x1B => {
                if self.text.take().is_some() {
                    return None;
                }
                return Some(Action::Close);
            }
            0x0D => {
                if self.text.is_some() {
                    self.commit_text();
                    return None;
                }
                if self.is_picker() || self.editing {
                    return Some(Action::Finish);
                }
            }
            0x5A if ctrl => {
                self.marks.pop();
            }
            0x43 if ctrl && self.editing => return Some(Action::Finish),
            // Стрелки двигают пипетку на одну точку
            0x25..=0x28 if self.is_picker() => {
                let (dx, dy) = match vk {
                    0x25 => (-1, 0),
                    0x26 => (0, -1),
                    0x27 => (1, 0),
                    _ => (0, 1),
                };
                let mut pt = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut pt);
                    let _ = SetCursorPos(pt.x + dx, pt.y + dy);
                }
            }
            _ => {}
        }
        None
    }

    fn char(&mut self, c: u32) {
        let Some((_, text)) = &mut self.text else { return };
        match c {
            8 => {
                text.pop();
            }
            0..=31 => {}
            _ => {
                if let Some(ch) = char::from_u32(c) {
                    text.push(ch);
                }
            }
        }
    }

    fn commit_text(&mut self) {
        if let Some((pos, text)) = self.text.take() {
            if !text.trim().is_empty() {
                self.marks.push(Mark::Text(pos, text, COLORS[self.color]));
            }
        }
    }

    fn layout_toolbar(&mut self) {
        let Some((sx, sy, sw, sh)) = self.sel else { return };
        let s = self.scale;
        let size = 34.0 * s;
        let gap = 4.0 * s;
        let pad = 6.0 * s;
        let total = BUTTONS.len() as f32 * (size + gap) - gap + pad * 2.0;
        let height = size + pad * 2.0;
        let screen_h = self.shot.h as f32;
        let mut y = sy + sh + 10.0 * s;
        if y + height > screen_h {
            y = sy - 10.0 * s - height;
        }
        if y < 0.0 {
            y = sy + sh - height - 10.0 * s;
        }
        let x = (sx + sw - total).max(0.0).min(self.shot.w as f32 - total);
        self.buttons.clear();
        // Фон панели — первой записью с пустой кнопкой не храним; рисуем по общей рамке
        for (i, b) in BUTTONS.iter().enumerate() {
            self.buttons.push((x + pad + i as f32 * (size + gap), y + pad, size, size, *b));
        }
    }

    fn draw_mark(c: &mut Canvas, mark: &Mark, s: f32) {
        match mark {
            Mark::Arrow(a, b, color) => {
                let w = 4.0 * s;
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let len = (dx * dx + dy * dy).sqrt().max(1.0);
                let (ux, uy) = (dx / len, dy / len);
                let head = (18.0 * s).min(len * 0.6);
                let base = (b.0 - ux * head * 0.8, b.1 - uy * head * 0.8);
                c.stroke_polyline(&[*a, base], w, *color);
                let (px, py) = (-uy, ux);
                c.fill_polygon(
                    &[*b, (b.0 - ux * head + px * head * 0.55, b.1 - uy * head + py * head * 0.55), (b.0 - ux * head - px * head * 0.55, b.1 - uy * head - py * head * 0.55)],
                    *color,
                );
            }
            Mark::Rect(a, b, color) => {
                let (x, y, w, h) = norm(*a, *b);
                c.stroke_round_rect(x, y, w, h, 3.0 * s, 3.5 * s, *color);
            }
            Mark::Marker(points, color) => {
                let marker = if *color == COLORS[4] || *color == COLORS[5] { COLORS[1] } else { *color };
                c.stroke_polyline(points, 18.0 * s, marker.with_alpha(0.42));
            }
            Mark::Text(pos, text, color) => {
                let size = 22.0 * s;
                let (_, lh) = crate::gfx::text::metrics(size, Weight::Semibold);
                // Тёмная подложка, чтобы текст читался на любом фоне
                let tw = crate::gfx::text::measure(text, Weight::Semibold, size);
                let back = if *color == COLORS[5] { Rgba::rgba(255, 255, 255, 0xB0) } else { Rgba::rgba(0, 0, 0, 0x80) };
                c.fill_round_rect(pos.0 - 4.0 * s, pos.1 - lh / 2.0 - 2.0 * s, tw + 8.0 * s, lh + 4.0 * s, 4.0 * s, back);
                c.text(text, Weight::Semibold, size, pos.0, pos.1 - lh / 2.0, *color);
            }
            Mark::Blur(..) => {}
        }
    }

    /// Готовая картинка выделенной области с разметкой.
    fn export(&self, rect: RECT) -> Shot {
        let mut out = self.shot.crop(rect);
        let ox = (rect.left - self.shot.x) as f32;
        let oy = (rect.top - self.shot.y) as f32;
        for m in &self.marks {
            if let Mark::Blur(a, b) = m {
                let (x, y, w, h) = norm(*a, *b);
                pixelate(&mut out.bgra, out.w, out.h, x - ox, y - oy, w, h, (12.0 * self.scale) as u32);
            }
        }
        let (ow, oh) = (out.w, out.h);
        if let Some(pm) = tiny_skia::PixmapMut::from_bytes(&mut out.bgra, ow, oh) {
            let mut c = Canvas::with_offset(pm, ox, oy);
            for m in &self.marks {
                Self::draw_mark(&mut c, m, self.scale);
            }
        }
        for p in out.bgra.chunks_exact_mut(4) {
            p[3] = 255;
        }
        out
    }

    fn render(&mut self) {
        let (w, h) = (self.shot.w, self.shot.h);
        let s = self.scale;
        let picker = self.is_picker();
        if picker {
            self.frame.copy_from_slice(&self.shot.bgra);
        } else {
            self.frame.copy_from_slice(&self.dim);
        }
        if let Some((sx, sy, sw, sh)) = self.sel {
            let x0 = (sx.max(0.0) as u32).min(w);
            let x1 = ((sx + sw).max(0.0) as u32).min(w);
            for row in (sy.max(0.0) as u32).min(h)..((sy + sh).max(0.0) as u32).min(h) {
                let a = ((row * w + x0) * 4) as usize;
                let b = ((row * w + x1) * 4) as usize;
                self.frame[a..b].copy_from_slice(&self.shot.bgra[a..b]);
            }
            for m in self.marks.iter().chain(self.current.iter()) {
                if let Mark::Blur(a, b) = m {
                    let (x, y, bw, bh) = norm(*a, *b);
                    pixelate(&mut self.frame, w, h, x, y, bw, bh, (12.0 * s) as u32);
                }
            }
        }
        let mut frame = std::mem::take(&mut self.frame);
        if let Some(pm) = tiny_skia::PixmapMut::from_bytes(&mut frame, w, h) {
            let mut c = Canvas::new(pm);
            self.draw_over(&mut c);
        }
        self.frame = frame;
    }

    fn draw_over(&self, c: &mut Canvas) {
        let s = self.scale;
        let picker = self.is_picker();
        let accent = crate::theme::accent();
        if let Some((sx, sy, sw, sh)) = self.sel {
            for m in self.marks.iter().chain(self.current.iter()) {
                Self::draw_mark(c, m, s);
            }
            if let Some((pos, text)) = &self.text {
                let shown = format!("{text}|");
                Self::draw_mark(c, &Mark::Text(*pos, shown, COLORS[self.color]), s);
            }
            if let Some(Mark::Blur(a, b)) = &self.current {
                let (x, y, bw, bh) = norm(*a, *b);
                c.stroke_rect(x, y, bw, bh, 1.0, Rgba::rgba(255, 255, 255, 0xA0));
            }
            c.stroke_rect(sx - 1.0, sy - 1.0, sw + 2.0, sh + 2.0, 2.0 * s, accent);
            let label = format!("{} × {}", sw.round() as i32, sh.round() as i32);
            let size = 12.0 * s;
            let lw = crate::gfx::text::measure(&label, Weight::Semibold, size) + 12.0 * s;
            let ly = if sy > 26.0 * s { sy - 26.0 * s } else { sy + 6.0 * s };
            c.fill_round_rect(sx, ly, lw, 20.0 * s, 6.0 * s, Rgba::rgba(0, 0, 0, 0xC0));
            c.text_centered(&label, Weight::Semibold, size, sx, ly, lw, 20.0 * s, Rgba::WHITE);
        } else if !picker {
            self.draw_hint(c);
        }
        if self.editing {
            self.draw_toolbar(c);
        }
        if picker {
            self.draw_magnifier(c);
        }
    }

    fn draw_hint(&self, c: &mut Canvas) {
        let s = self.scale;
        let pt = POINT { x: self.shot.x + self.cursor.0 as i32, y: self.shot.y + self.cursor.1 as i32 };
        let m = win::monitor_at(pt).bounds;
        let text = if self.is_screenshot() {
            tr("Выделите область или щёлкните для всего экрана · Esc — отмена", "Select an area or click for the whole screen · Esc to cancel")
        } else {
            tr("Выделите область · Esc — отмена", "Select an area · Esc to cancel")
        };
        let size = 14.0 * s;
        let tw = crate::gfx::text::measure(text, Weight::Semibold, size) + 32.0 * s;
        let x = (m.left - self.shot.x) as f32 + ((m.right - m.left) as f32 - tw) / 2.0;
        let y = (m.top - self.shot.y) as f32 + 60.0 * s;
        c.fill_round_rect(x, y, tw, 36.0 * s, 18.0 * s, Rgba::rgba(0, 0, 0, 0xD0));
        c.text_centered(text, Weight::Semibold, size, x, y, tw, 36.0 * s, Rgba::WHITE);
    }

    fn draw_toolbar(&self, c: &mut Canvas) {
        let s = self.scale;
        let (Some(first), Some(last)) = (self.buttons.first(), self.buttons.last()) else { return };
        let pad = 6.0 * s;
        let (x, y) = (first.0 - pad, first.1 - pad);
        let (w, h) = (last.0 + last.2 - first.0 + pad * 2.0, first.3 + pad * 2.0);
        c.fill_round_rect(x, y, w, h, 12.0 * s, Rgba::rgba(0x1C, 0x1C, 0x1E, 0xF2));
        c.stroke_round_rect(x + 0.5, y + 0.5, w - 1.0, h - 1.0, 12.0 * s, 1.0, Rgba::rgba(255, 255, 255, 0x26));
        let accent = crate::theme::accent();
        for (bx, by, bw, bh, b) in &self.buttons {
            let active = matches!(b, Button::Tool(t) if *t == self.tool);
            let hover = self.hover == Some(*b);
            if active {
                c.fill_round_rect(*bx, *by, *bw, *bh, 8.0 * s, accent.with_alpha(0.35));
            } else if hover {
                c.fill_round_rect(*bx, *by, *bw, *bh, 8.0 * s, Rgba::rgba(255, 255, 255, 0x22));
            }
            let (cx, cy) = (bx + bw / 2.0, by + bh / 2.0);
            let white = Rgba::WHITE;
            let k = 8.0 * s;
            match b {
                Button::Tool(Tool::Arrow) => Self::draw_mark(c, &Mark::Arrow((cx - k, cy + k), (cx + k, cy - k), white), s * 0.6),
                Button::Tool(Tool::Rect) => c.stroke_round_rect(cx - k, cy - k * 0.75, k * 2.0, k * 1.5, 2.0 * s, 2.0 * s, white),
                Button::Tool(Tool::Marker) => c.stroke_polyline(&[(cx - k, cy + 3.0 * s), (cx + k, cy - 3.0 * s)], 7.0 * s, COLORS[1].with_alpha(0.8)),
                Button::Tool(Tool::Text) => c.text_centered("T", Weight::Bold, 17.0 * s, *bx, *by, *bw, *bh, white),
                Button::Tool(Tool::Blur) => {
                    for i in 0..3 {
                        for j in 0..3 {
                            let shade = if (i + j) % 2 == 0 { 0xFF } else { 0x70 };
                            c.fill_rect(cx - k + i as f32 * k * 0.67, cy - k + j as f32 * k * 0.67, k * 0.67, k * 0.67, Rgba::rgba(255, 255, 255, shade));
                        }
                    }
                }
                Button::Color => {
                    c.fill_circle(cx, cy, 9.0 * s, COLORS[self.color]);
                    c.stroke_circle(cx, cy, 9.0 * s, 1.5 * s, Rgba::rgba(255, 255, 255, 0x90));
                }
                Button::Undo => c.icon('\u{E7A7}', 15.0 * s, cx, cy, white),
                Button::Done => c.icon('\u{E73E}', 16.0 * s, cx, cy, Rgba::rgb(0x34, 0xC7, 0x59)),
                Button::Close => c.icon('\u{E711}', 14.0 * s, cx, cy, white),
            }
        }
    }

    fn draw_magnifier(&self, c: &mut Canvas) {
        let s = self.scale;
        let n = 15i32;
        let cell = (9.0 * s).round();
        let size = cell * n as f32;
        let (mx, my) = self.cursor;
        let mut x = mx + 24.0 * s;
        let mut y = my + 24.0 * s;
        if x + size > self.shot.w as f32 {
            x = mx - 24.0 * s - size;
        }
        if y + size + 48.0 * s > self.shot.h as f32 {
            y = my - 24.0 * s - size - 48.0 * s;
        }
        c.fill_round_rect(x - 4.0 * s, y - 4.0 * s, size + 8.0 * s, size + 52.0 * s, 10.0 * s, Rgba::rgba(0x1C, 0x1C, 0x1E, 0xF2));
        let sx = self.shot.x + mx as i32;
        let sy = self.shot.y + my as i32;
        for j in 0..n {
            for i in 0..n {
                let rgb = self.shot.pixel(sx + i - n / 2, sy + j - n / 2).unwrap_or([0, 0, 0]);
                c.fill_rect(x + i as f32 * cell, y + j as f32 * cell, cell, cell, Rgba::rgb(rgb[0], rgb[1], rgb[2]));
            }
        }
        let center = (n / 2) as f32 * cell;
        c.stroke_rect(x + center, y + center, cell, cell, 1.5 * s, Rgba::WHITE);
        let rgb = self.shot.pixel(sx, sy).unwrap_or([0, 0, 0]);
        let hex = format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
        c.fill_round_rect(x, y + size + 8.0 * s, 20.0 * s, 20.0 * s, 5.0 * s, Rgba::rgb(rgb[0], rgb[1], rgb[2]));
        c.text(&hex, Weight::Semibold, 14.0 * s, x + 28.0 * s, y + size + 7.0 * s, Rgba::WHITE);
        let rgb_text = format!("{}, {}, {}", rgb[0], rgb[1], rgb[2]);
        c.text(&rgb_text, Weight::Regular, 11.5 * s, x + 28.0 * s, y + size + 26.0 * s, Rgba::rgba(255, 255, 255, 0xA0));
    }
}

/// Размытие личных данных: крупные квадраты, сквозь которые текст не прочитать.
fn pixelate(buf: &mut [u8], bw: u32, bh: u32, x: f32, y: f32, w: f32, h: f32, block: u32) {
    let block = block.max(4);
    let x0 = x.max(0.0) as u32;
    let y0 = y.max(0.0) as u32;
    let x1 = ((x + w).max(0.0) as u32).min(bw);
    let y1 = ((y + h).max(0.0) as u32).min(bh);
    let mut by = y0;
    while by < y1 {
        let mut bx = x0;
        let ey = (by + block).min(y1);
        while bx < x1 {
            let ex = (bx + block).min(x1);
            let mut sum = [0u32; 3];
            let mut count = 0u32;
            for yy in by..ey {
                for xx in bx..ex {
                    let i = ((yy * bw + xx) * 4) as usize;
                    sum[0] += buf[i] as u32;
                    sum[1] += buf[i + 1] as u32;
                    sum[2] += buf[i + 2] as u32;
                    count += 1;
                }
            }
            if count > 0 {
                for yy in by..ey {
                    for xx in bx..ex {
                        let i = ((yy * bw + xx) * 4) as usize;
                        buf[i] = (sum[0] / count) as u8;
                        buf[i + 1] = (sum[1] / count) as u8;
                        buf[i + 2] = (sum[2] / count) as u8;
                    }
                }
            }
            bx = ex;
        }
        by = ey;
    }
}
