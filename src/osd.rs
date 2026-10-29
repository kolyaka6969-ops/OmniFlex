//! Всплывающий индикатор громкости внизу экрана: не забирает фокус и пропускает щелчки.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, SetTimer, SetWindowPos, ShowWindow, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE,
    SW_SHOWNOACTIVATE, WM_TIMER, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP,
};

use crate::audio::Channel;
use crate::gfx::{self, Canvas, Weight};
use crate::process_info::AppIcon;
use crate::wnd::{self, Handler};
use crate::{theme, win};

const SHORT: Duration = Duration::from_millis(1600);
const FADE: f32 = 0.18;
const TIMER_FRAME: usize = 1;

#[derive(Clone)]
struct Content {
    glyph: Option<char>,
    icon: Option<std::sync::Arc<AppIcon>>,
    name: String,
    value: Option<(f32, bool)>,
}

struct Osd {
    hwnd: HWND,
    content: Option<Content>,
    shown_at: Instant,
    hide_at: Instant,
    visible: bool,
}

thread_local! {
    static OSD: RefCell<Option<Rc<RefCell<Osd>>>> = const { RefCell::new(None) };
}

struct OsdHandler;

impl Handler for OsdHandler {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, _lparam: LPARAM) -> Option<LRESULT> {
        if msg == WM_TIMER && wparam.0 == TIMER_FRAME {
            if let Some(osd) = get() {
                if let Ok(mut o) = osd.try_borrow_mut() {
                    o.frame();
                }
            } else {
                unsafe {
                    let _ = KillTimer(hwnd, TIMER_FRAME);
                }
            }
            return Some(LRESULT(0));
        }
        None
    }
}

fn get() -> Option<Rc<RefCell<Osd>>> {
    OSD.with(|o| {
        if o.borrow().is_none() {
            let hwnd = wnd::create(
                "OmniFlexOsd",
                "OmniFlex OSD",
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                WS_POPUP,
                None,
                (0, 0, 1, 1),
                Rc::new(RefCell::new(OsdHandler)),
            )?;
            *o.borrow_mut() = Some(Rc::new(RefCell::new(Osd {
                hwnd,
                content: None,
                shown_at: Instant::now(),
                hide_at: Instant::now(),
                visible: false,
            })));
        }
        o.borrow().clone()
    })
}

pub fn show_channel(channel: &Channel) {
    let glyph = if channel.muted {
        Some('\u{E74F}')
    } else if channel.icon.is_none() {
        Some(if channel.is_system { '\u{E7F5}' } else { channel.glyph() })
    } else {
        None
    };
    show(Content {
        glyph,
        icon: if glyph.is_none() { channel.icon.clone() } else { None },
        name: channel.name.clone(),
        value: Some((channel.volume, channel.muted)),
    });
}

pub fn show_message(glyph: char, text: &str) {
    show(Content { glyph: Some(glyph), icon: None, name: text.to_string(), value: None });
}

fn show(content: Content) {
    let Some(osd) = get() else { return };
    let mut o = osd.borrow_mut();
    o.content = Some(content);
    if !o.visible {
        o.shown_at = Instant::now();
    }
    o.visible = true;
    o.hide_at = Instant::now() + SHORT;
    o.render();
    unsafe {
        let _ = ShowWindow(o.hwnd, SW_SHOWNOACTIVATE);
        let _ = SetWindowPos(o.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        SetTimer(o.hwnd, TIMER_FRAME, 16, None);
    }
}

impl Osd {
    fn frame(&mut self) {
        if !self.visible {
            return;
        }
        let now = Instant::now();
        if now >= self.hide_at + Duration::from_secs_f32(FADE) {
            self.visible = false;
            unsafe {
                let _ = KillTimer(self.hwnd, TIMER_FRAME);
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
            return;
        }
        // Пока индикатор стоит, кадры не нужны: рисуем только появление и затухание
        let appearing = now.duration_since(self.shown_at).as_secs_f32() < FADE;
        if appearing || now >= self.hide_at {
            self.render();
        }
    }

    fn opacity(&self) -> f32 {
        let now = Instant::now();
        let appear = (now.duration_since(self.shown_at).as_secs_f32() / FADE).min(1.0);
        let fade = if now >= self.hide_at { 1.0 - (now.duration_since(self.hide_at).as_secs_f32() / FADE).min(1.0) } else { 1.0 };
        appear.min(fade)
    }

    fn render(&mut self) {
        let Some(content) = self.content.clone() else { return };
        let monitor = win::primary_monitor();
        let scale = win::monitor_scale(&monitor);
        let pal = theme::palette();
        let w = (300.0 * scale).round();
        let h = (64.0 * scale).round();
        let pad = (16.0 * scale).round();
        let mut pm = gfx::pixmap((w + pad * 2.0) as u32, (h + pad * 2.0) as u32);
        {
            let mut c = Canvas::new(pm.as_mut());
            c.alpha = self.opacity();
            let s = scale;
            // Мягкая тень
            let (shadow, sp) = gfx::make_shadow(w, h, 12.0 * s, 12.0 * s, 0.35);
            c.draw_pixmap(&shadow, pad - sp, pad - sp + 3.0 * s, 1.0);
            c.fill_round_rect(pad, pad, w, h, 12.0 * s, pal.background);
            c.stroke_round_rect(pad + 0.5, pad + 0.5, w - 1.0, h - 1.0, 12.0 * s, 1.0, pal.border);

            let icon_cx = pad + 32.0 * s;
            let cy = pad + h / 2.0;
            if let Some(icon) = &content.icon {
                if let Some(img) = gfx::icon_to_pixmap(icon.width, icon.height, &icon.rgba) {
                    c.draw_image_scaled(&img, icon_cx - 12.0 * s, cy - 12.0 * s, 24.0 * s, 24.0 * s, 1.0);
                }
            } else if let Some(g) = content.glyph {
                c.icon(g, 20.0 * s, icon_cx, cy, pal.text);
            }
            let text_x = pad + 58.0 * s;
            let right = pad + w - 16.0 * s;
            match content.value {
                Some((volume, muted)) => {
                    let value_text = if muted { String::new() } else { format!("{}", volume as i32) };
                    let value_w = gfx::text::measure(&value_text, Weight::Semibold, 15.0 * s);
                    let name = gfx::text::ellipsize(&content.name, Weight::Regular, 13.0 * s, right - text_x - value_w - 8.0 * s);
                    c.draw_line(&name, text_x, pad + 11.0 * s, pal.secondary);
                    c.text(&value_text, Weight::Semibold, 15.0 * s, right - value_w, pad + 9.0 * s, pal.text);
                    let bar_y = pad + 40.0 * s;
                    let bar_w = right - text_x;
                    c.fill_round_rect(text_x, bar_y, bar_w, 4.0 * s, 2.0 * s, pal.track);
                    let fill = pal.accent.with_alpha(if muted { 0.4 } else { 1.0 });
                    c.fill_round_rect(text_x, bar_y, bar_w * volume / 100.0, 4.0 * s, 2.0 * s, fill);
                }
                None => {
                    let name = gfx::text::ellipsize(&content.name, Weight::Semibold, 14.0 * s, right - text_x);
                    let (_, lh) = gfx::text::metrics(14.0 * s, Weight::Semibold);
                    c.draw_line(&name, text_x, cy - lh / 2.0, pal.text);
                }
            }
        }
        let area = monitor.work;
        let x = area.left + ((area.right - area.left) - pm.width() as i32) / 2;
        let y = area.bottom - pm.height() as i32 - (24.0 * scale) as i32 + pad as i32;
        wnd::update_layered(self.hwnd, Some((x, y)), &pm);
    }
}
