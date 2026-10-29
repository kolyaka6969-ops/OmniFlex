//! Компактный микшер, который открывается по щелчку на значке в трее (и горячей клавишей «Открыть микшер»).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, PostMessageW, SetForegroundWindow,
    ShowWindow, TrackPopupMenu, MF_CHECKED, MF_STRING, SW_HIDE, SW_SHOW, TPM_RETURNCMD, TPM_TOPALIGN, WM_ACTIVATE,
    WM_CLOSE, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NULL,
    WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::audio::{self, Channel, Target};
use crate::gfx::{self, Canvas, Rgba, Weight};
use crate::shell;
use crate::wnd::{self, Handler};
use windows::Win32::Graphics::Gdi::ClientToScreen;

const WM_MOUSELEAVE: u32 = 0x02A3;
use crate::{theme, util, win};

const WIDTH: f32 = 360.0;
const PAD: f32 = 14.0;
const HEADER: f32 = 58.0;
const ROW: f32 = 54.0;
const FOOTER: f32 = 48.0;
const MAX_ROWS: usize = 7;
const SHADOW: f32 = 18.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    None,
    Device,
    Mute(usize),
    Slider(usize),
    Row(usize),
    Hotkeys,
    Settings,
    Mixer,
}

struct Flyout {
    hwnd: HWND,
    visible: bool,
    hidden_at: Option<Instant>,
    scale: f32,
    pos: (i32, i32),
    size: (u32, u32),
    hover: Hit,
    drag: Option<usize>,
    scroll: usize,
    /// Каналы на момент последней отрисовки: 0 — общая громкость, дальше приложения.
    channels: Vec<(Target, Channel)>,
    tracking: bool,
}

thread_local! {
    static FLYOUT: RefCell<Option<Rc<RefCell<Flyout>>>> = const { RefCell::new(None) };
}

fn get() -> Option<Rc<RefCell<Flyout>>> {
    FLYOUT.with(|f| {
        if f.borrow().is_none() {
            let hwnd = wnd::create(
                "OmniFlexFlyout",
                "OmniFlex — громкость",
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                WS_POPUP,
                None,
                (0, 0, 1, 1),
                Rc::new(RefCell::new(FlyoutHandler)),
            )?;
            *f.borrow_mut() = Some(Rc::new(RefCell::new(Flyout {
                hwnd,
                visible: false,
                hidden_at: None,
                scale: 1.0,
                pos: (0, 0),
                size: (1, 1),
                hover: Hit::None,
                drag: None,
                scroll: 0,
                channels: Vec::new(),
                tracking: false,
            })));
        }
        f.borrow().clone()
    })
}

pub fn is_visible() -> bool {
    FLYOUT.with(|f| f.borrow().as_ref().map(|f| f.try_borrow().map(|f| f.visible).unwrap_or(true)).unwrap_or(false))
}

pub fn toggle() {
    let Some(fly) = get() else { return };
    let (visible, just_hidden) = {
        let f = fly.borrow();
        (f.visible, f.hidden_at.map(|t| t.elapsed().as_millis() < 350).unwrap_or(false))
    };
    if visible {
        hide();
    } else if !just_hidden {
        show();
    }
}

pub fn show() {
    let Some(fly) = get() else { return };
    shell::with_mixer(|m| {
        m.refresh_devices();
        m.refresh();
    });
    let hwnd = {
        let mut f = fly.borrow_mut();
        f.visible = true;
        f.scroll = 0;
        f.hover = Hit::None;
        f.render(true);
        f.hwnd
    };
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

pub fn hide() {
    let Some(fly) = get() else { return };
    let Ok(mut f) = fly.try_borrow_mut() else { return };
    if !f.visible {
        return;
    }
    f.visible = false;
    f.drag = None;
    f.hidden_at = Some(Instant::now());
    unsafe {
        let _ = ShowWindow(f.hwnd, SW_HIDE);
    }
}

pub fn mixer_changed() {
    if let Some(fly) = get() {
        if let Ok(mut f) = fly.try_borrow_mut() {
            if f.visible {
                f.render(false);
            }
        }
    }
}

pub fn theme_changed() {
    mixer_changed();
}

struct FlyoutHandler;

impl Handler for FlyoutHandler {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_ACTIVATE => {
                // Окно потеряло фокус — прячем, как системные всплывающие окна
                if wparam.0 & 0xFFFF == 0 {
                    hide();
                }
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                hide();
                Some(LRESULT(0))
            }
            WM_KEYDOWN => {
                if wparam.0 == 0x1B {
                    hide();
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSELEAVE | WM_MOUSEWHEEL => {
                let action = {
                    let Some(fly) = get() else { return None };
                    let Ok(mut f) = fly.try_borrow_mut() else { return None };
                    f.mouse(hwnd, msg, wparam, lparam)
                };
                // Меню и окна открываем вне заимствования: у них свои циклы сообщений
                match action {
                    Some(Hit::Device) => device_menu(hwnd),
                    Some(Hit::Hotkeys) => {
                        hide();
                        crate::ui::show_main(crate::ui::Page::Hotkeys);
                    }
                    Some(Hit::Settings) => {
                        hide();
                        crate::ui::show_main(crate::ui::Page::Settings);
                    }
                    Some(Hit::Mixer) => {
                        hide();
                        crate::ui::show_main(crate::ui::Page::Mixer);
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

fn device_menu(hwnd: HWND) {
    let snap = audio::snapshot();
    if snap.devices.is_empty() {
        return;
    }
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        for (i, d) in snap.devices.iter().enumerate() {
            let w = util::wide(&d.name);
            let flags = if d.id == snap.device_id { MF_STRING | MF_CHECKED } else { MF_STRING };
            let _ = AppendMenuW(menu, flags, i + 1, PCWSTR(w.as_ptr()));
        }
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_TOPALIGN, pt.x, pt.y, 0, hwnd, None);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        if cmd.0 > 0 {
            if let Some(d) = snap.devices.get(cmd.0 as usize - 1) {
                let id = d.id.clone();
                shell::with_mixer(|m| m.set_device(&id));
                shell::after_mixer_change();
            }
        }
        let _ = SetForegroundWindow(hwnd);
    }
}

impl Flyout {
    fn rows_visible(&self) -> usize {
        self.channels.len().saturating_sub(1).min(MAX_ROWS)
    }

    fn layout_height(&self) -> f32 {
        let apps = self.rows_visible();
        let apps_block = if apps == 0 { 40.0 } else { 26.0 + apps as f32 * ROW };
        PAD + HEADER + ROW + 8.0 + apps_block + FOOTER + PAD
    }

    /// Строка канала по индексу в channels: где она по вертикали (в логических точках).
    fn row_top(&self, index: usize) -> Option<f32> {
        if index == 0 {
            return Some(PAD + HEADER);
        }
        let visible = index.checked_sub(1 + self.scroll)?;
        if visible >= MAX_ROWS {
            return None;
        }
        Some(PAD + HEADER + ROW + 8.0 + 26.0 + visible as f32 * ROW)
    }

    fn hit(&self, x: f32, y: f32) -> Hit {
        if y >= PAD && y < PAD + HEADER && x >= PAD && x < WIDTH - PAD {
            return Hit::Device;
        }
        for i in 0..self.channels.len() {
            let Some(top) = self.row_top(i) else { continue };
            if y >= top && y < top + ROW {
                if x >= PAD && x < PAD + 44.0 {
                    return Hit::Mute(i);
                }
                if x >= PAD + 52.0 && x < WIDTH - PAD - 40.0 && y >= top + 22.0 {
                    return Hit::Slider(i);
                }
                return Hit::Row(i);
            }
        }
        let footer_top = self.layout_height() - PAD - FOOTER;
        if y >= footer_top + 8.0 && y < footer_top + FOOTER {
            let third = (WIDTH - PAD * 2.0) / 3.0;
            let col = ((x - PAD) / third).floor();
            return match col as i32 {
                0 => Hit::Mixer,
                1 => Hit::Hotkeys,
                2 => Hit::Settings,
                _ => Hit::None,
            };
        }
        Hit::None
    }

    fn slider_value(&self, x: f32) -> f32 {
        let left = PAD + 52.0;
        let right = WIDTH - PAD - 40.0;
        ((x - left) / (right - left) * 100.0).clamp(0.0, 100.0)
    }

    fn mouse(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<Hit> {
        let (px, py) = wnd::point_from_lparam(lparam);
        let s = self.scale;
        let (mut x, mut y) = ((px as f32) / s - SHADOW, (py as f32) / s - SHADOW);
        match msg {
            WM_MOUSEWHEEL => {
                // Колёсико приходит в экранных координатах
                let mut origin = POINT::default();
                unsafe {
                    let _ = ClientToScreen(hwnd, &mut origin);
                }
                x = (px - origin.x) as f32 / s - SHADOW;
                y = (py - origin.y) as f32 / s - SHADOW;
                let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16;
                let up = delta > 0;
                match self.hit(x, y) {
                    Hit::Slider(i) | Hit::Mute(i) | Hit::Row(i) if x >= PAD + 52.0 || i == 0 => {
                        let step = crate::settings::read(|s| s.volume_step) as f32;
                        if let Some((target, ch)) = self.channels.get(i).cloned() {
                            let v = (ch.volume + if up { step } else { -step }).clamp(0.0, 100.0);
                            shell::with_mixer(|m| m.set_volume(&target, v));
                        }
                    }
                    _ => {
                        // Над названиями — прокрутка списка приложений
                        let apps = self.channels.len().saturating_sub(1);
                        if apps > MAX_ROWS {
                            if up && self.scroll > 0 {
                                self.scroll -= 1;
                            } else if !up && self.scroll + MAX_ROWS < apps {
                                self.scroll += 1;
                            }
                        }
                    }
                }
                self.render(false);
                return None;
            }
            WM_MOUSEMOVE => {
                if !self.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut tme);
                    }
                    self.tracking = true;
                }
                if let Some(i) = self.drag {
                    if let Some((target, _)) = self.channels.get(i).cloned() {
                        let v = self.slider_value(x);
                        shell::with_mixer(|m| m.set_volume(&target, v));
                        if let Some(c) = self.channels.get_mut(i) {
                            c.1.volume = v;
                        }
                    }
                    self.render(false);
                    return None;
                }
                let hover = self.hit(x, y);
                if hover != self.hover {
                    self.hover = hover;
                    self.render(false);
                }
                None
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                if self.hover != Hit::None {
                    self.hover = Hit::None;
                    self.render(false);
                }
                None
            }
            WM_LBUTTONDOWN => {
                let hit = self.hit(x, y);
                match hit {
                    Hit::Slider(i) => {
                        self.drag = Some(i);
                        unsafe {
                            SetCapture(hwnd);
                        }
                        if let Some((target, ch)) = self.channels.get(i).cloned() {
                            let v = self.slider_value(x);
                            shell::with_mixer(|m| {
                                if ch.muted {
                                    m.set_mute(&target, false);
                                }
                                m.set_volume(&target, v)
                            });
                        }
                        self.render(false);
                        None
                    }
                    Hit::Mute(i) => {
                        if let Some((target, ch)) = self.channels.get(i).cloned() {
                            shell::with_mixer(|m| m.set_mute(&target, !ch.muted));
                        }
                        self.render(false);
                        None
                    }
                    _ => None,
                }
            }
            WM_LBUTTONUP => {
                if self.drag.take().is_some() {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                    shell::after_mixer_change();
                    return None;
                }
                match self.hit(x, y) {
                    h @ (Hit::Device | Hit::Hotkeys | Hit::Settings | Hit::Mixer) => Some(h),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn render(&mut self, place: bool) {
        let snap = audio::snapshot();
        let mut channels = vec![(Target::Master, snap.master.clone())];
        for app in &snap.apps {
            channels.push((Target::App(app.key.clone()), app.clone()));
        }
        // Пока тянут ползунок, показываем значение под мышью, а не запоздалое из снимка
        if let Some(i) = self.drag {
            if let (Some(old), Some(new)) = (self.channels.get(i), channels.get_mut(i)) {
                new.1.volume = old.1.volume;
            }
        }
        self.channels = channels;
        let apps = self.channels.len().saturating_sub(1);
        if self.scroll + MAX_ROWS > apps {
            self.scroll = apps.saturating_sub(MAX_ROWS);
        }

        let monitor = if place || self.size.0 <= 1 {
            let mut pt = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut pt);
            }
            win::monitor_at(pt)
        } else {
            win::monitor_of_window(self.hwnd)
        };
        let s = win::monitor_scale(&monitor);
        self.scale = s;
        let h = self.layout_height();
        let w_px = ((WIDTH + SHADOW * 2.0) * s).ceil() as u32;
        let h_px = ((h + SHADOW * 2.0) * s).ceil() as u32;
        let pal = theme::palette();
        let mut pm = gfx::pixmap(w_px, h_px);
        {
            let mut c = Canvas::new(pm.as_mut());
            let o = SHADOW * s;
            let (shadow, sp) = gfx::make_shadow(WIDTH * s, h * s, 10.0 * s, 16.0 * s, 0.4);
            c.draw_pixmap(&shadow, o - sp, o - sp + 4.0 * s, 1.0);
            c.fill_round_rect(o, o, WIDTH * s, h * s, 10.0 * s, pal.background);
            c.stroke_round_rect(o + 0.5, o + 0.5, WIDTH * s - 1.0, h * s - 1.0, 10.0 * s, 1.0, pal.border);
            let px = |v: f32| o + v * s;

            // Заголовок: устройство вывода
            if self.hover == Hit::Device {
                c.fill_round_rect(px(PAD - 6.0), px(PAD), (WIDTH - PAD * 2.0 + 12.0) * s, (HEADER - 8.0) * s, 6.0 * s, pal.hover);
            }
            let device_glyph = snap.devices.iter().find(|d| d.id == snap.device_id).map(|d| d.glyph()).unwrap_or('\u{E7F5}');
            c.icon(device_glyph, 18.0 * s, px(PAD + 16.0), px(PAD + 25.0), pal.text);
            c.text("Устройство вывода", Weight::Regular, 11.5 * s, px(PAD + 40.0), px(PAD + 6.0), pal.secondary);
            let name = gfx::text::ellipsize(&snap.device_name, Weight::Semibold, 13.5 * s, (WIDTH - PAD * 2.0 - 70.0) * s);
            c.draw_line(&name, px(PAD + 40.0), px(PAD + 23.0), pal.text);
            if snap.devices.len() > 1 {
                c.icon('\u{E70D}', 11.0 * s, px(WIDTH - PAD - 12.0), px(PAD + 25.0), pal.secondary);
            }

            for i in 0..self.channels.len() {
                let Some(top) = self.row_top(i) else { continue };
                let (_, ch) = &self.channels[i];
                self.draw_row(&mut c, ch, i, o, top, s, &pal);
            }
            if apps == 0 {
                c.text(
                    "Сейчас ни одно приложение не воспроизводит звук",
                    Weight::Regular,
                    12.0 * s,
                    px(PAD + 4.0),
                    px(PAD + HEADER + ROW + 14.0),
                    pal.secondary,
                );
            } else {
                c.text("Приложения", Weight::Semibold, 12.0 * s, px(PAD + 4.0), px(PAD + HEADER + ROW + 10.0), pal.secondary);
                if apps > MAX_ROWS {
                    let label = format!("{}–{} из {}", self.scroll + 1, self.scroll + MAX_ROWS, apps);
                    let lw = gfx::text::measure(&label, Weight::Regular, 11.0 * s);
                    c.text(&label, Weight::Regular, 11.0 * s, px(WIDTH - PAD) - lw, px(PAD + HEADER + ROW + 11.0), pal.secondary);
                }
            }

            // Кнопки внизу
            let footer_top = h - PAD - FOOTER;
            c.fill_rect(px(PAD), px(footer_top), (WIDTH - PAD * 2.0) * s, 1.0, pal.border);
            let third = (WIDTH - PAD * 2.0) / 3.0;
            for (k, (hit, glyph, label)) in [
                (Hit::Mixer, '\u{E9E9}', "Микшер"),
                (Hit::Hotkeys, '\u{E765}', "Клавиши"),
                (Hit::Settings, '\u{E713}', "Настройки"),
            ]
            .iter()
            .enumerate()
            {
                let bx = PAD + third * k as f32;
                if self.hover == *hit {
                    c.fill_round_rect(px(bx + 2.0), px(footer_top + 8.0), (third - 4.0) * s, (FOOTER - 10.0) * s, 6.0 * s, pal.hover);
                }
                let lw = gfx::text::measure(label, Weight::Regular, 12.0 * s);
                let total = 16.0 * s + 6.0 * s + lw;
                let start = px(bx) + (third * s - total) / 2.0;
                c.icon(*glyph, 14.0 * s, start + 8.0 * s, px(footer_top + 27.0), pal.text);
                c.text(label, Weight::Regular, 12.0 * s, start + 22.0 * s, px(footer_top + 18.0), pal.text);
            }
        }

        if place || self.size.0 <= 1 {
            let work = monitor.work;
            let x = work.right - w_px as i32 + ((SHADOW - 12.0) * s) as i32;
            let y = work.bottom - h_px as i32 + ((SHADOW - 12.0) * s) as i32;
            self.pos = (x, y);
        } else if h_px != self.size.1 {
            // Список приложений поменялся — растём вверх, низ остаётся у панели задач
            self.pos.1 -= h_px as i32 - self.size.1 as i32;
        }
        self.size = (w_px, h_px);
        wnd::update_layered(self.hwnd, Some(self.pos), &pm);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_row(&self, c: &mut Canvas, ch: &Channel, i: usize, o: f32, top: f32, s: f32, pal: &theme::Palette) {
        let px = |v: f32| o + v * s;
        if matches!(self.hover, Hit::Row(k) | Hit::Slider(k) | Hit::Mute(k) if k == i) {
            c.fill_round_rect(px(PAD - 6.0), px(top + 2.0), (WIDTH - PAD * 2.0 + 12.0) * s, (ROW - 4.0) * s, 6.0 * s, pal.hover);
        }
        // Значок: щелчок — без звука
        let icon_cx = px(PAD + 18.0);
        let icon_cy = px(top + ROW / 2.0);
        if self.hover == Hit::Mute(i) {
            c.fill_round_rect(px(PAD), px(top + 9.0), 36.0 * s, 36.0 * s, 6.0 * s, pal.hover);
        }
        let mut drew_icon = false;
        if !ch.muted {
            if let Some(icon) = &ch.icon {
                if let Some(img) = gfx::icon_to_pixmap(icon.width, icon.height, &icon.rgba) {
                    c.draw_image_scaled(&img, icon_cx - 12.0 * s, icon_cy - 12.0 * s, 24.0 * s, 24.0 * s, 1.0);
                    drew_icon = true;
                }
            }
        }
        if !drew_icon {
            let glyph = if ch.muted {
                '\u{E74F}'
            } else if i == 0 {
                ch.glyph()
            } else if ch.is_system {
                '\u{E7F5}'
            } else {
                '\u{E71D}'
            };
            c.icon(glyph, 18.0 * s, icon_cx, icon_cy, if ch.muted { pal.secondary } else { pal.text });
        }

        let name = gfx::text::ellipsize(&ch.name, Weight::Regular, 12.5 * s, (WIDTH - PAD * 2.0 - 100.0) * s);
        c.draw_line(&name, px(PAD + 52.0), px(top + 6.0), pal.text);

        // Ползунок с индикатором уровня
        let left = PAD + 52.0;
        let right = WIDTH - PAD - 40.0;
        let ty = top + 36.0;
        let track_w = right - left;
        c.fill_round_rect(px(left), px(ty - 2.0), track_w * s, 4.0 * s, 2.0 * s, pal.track);
        let peak_w = track_w * (ch.peak * ch.volume / 100.0).clamp(0.0, 1.0);
        if peak_w > 0.5 && !ch.muted {
            c.fill_round_rect(px(left), px(ty + 4.0), peak_w * s, 2.0 * s, 1.0 * s, pal.peak);
        }
        let fill = track_w * ch.volume / 100.0;
        let accent = if ch.muted { pal.secondary } else { pal.accent };
        c.fill_round_rect(px(left), px(ty - 2.0), fill * s, 4.0 * s, 2.0 * s, accent);
        let knob_x = px(left + fill);
        let active = self.drag == Some(i) || self.hover == Hit::Slider(i);
        c.fill_circle(knob_x, px(ty), 8.0 * s, if theme::is_dark() { Rgba::rgb(0x45, 0x45, 0x45) } else { Rgba::WHITE });
        c.stroke_circle(knob_x, px(ty), 8.0 * s, 1.0, pal.border);
        c.fill_circle(knob_x, px(ty), if active { 5.0 * s } else { 4.0 * s }, accent);

        let value = format!("{}", ch.volume as i32);
        let vw = gfx::text::measure(&value, Weight::Semibold, 12.5 * s);
        c.text(&value, Weight::Semibold, 12.5 * s, px(WIDTH - PAD) - vw, px(ty - 9.0), pal.text);
    }
}
