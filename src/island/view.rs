//! Рисование острова: свёрнутая пилюля, музыка, карточки, раскрытая панель с вкладками. Всё рисуется в
//! логических точках через `P`, который сам умножает на масштаб; заодно собирает области для щелчков.

use std::time::Duration;

use super::cards::{Card, GREEN, ORANGE, PURPLE, RED};
use super::{format_left, format_time, timer_presets, Activity, Hit, Island, Mode, Tool, ACTIVITY_H, CARD_W, COLLAPSED_H, EXP_W, PAD, TOP};
use crate::agents::Status;
use crate::audio;
use crate::gfx::{self, text, Canvas, Rgba, Weight};
use crate::i18n::tr;
use crate::settings::{self, EqStyle, ThemeMode};
use crate::sysinfo::Battery;
use crate::{launcher, util};

const LYRIC_H: f32 = 30.0;
const LYRIC_GAP: f32 = 6.0;

#[derive(Clone, Copy)]
pub(crate) struct Pal {
    pub bg: Rgba,
    pub fg: Rgba,
    pub dim: Rgba,
    pub faint: Rgba,
    pub chip: Rgba,
    pub chip_hover: Rgba,
    pub outline: Rgba,
    pub accent: Rgba,
}

pub(crate) fn palette(theme: ThemeMode) -> Pal {
    let light = match theme {
        ThemeMode::Light => true,
        ThemeMode::Dark => false,
        ThemeMode::System => crate::win::system_light_theme(),
    };
    let accent = crate::theme::accent();
    if light {
        Pal {
            bg: Rgba::rgb(0xF5, 0xF5, 0xF7),
            fg: Rgba::rgb(0x11, 0x11, 0x13),
            dim: Rgba::rgba(0, 0, 0, 0xBB),
            faint: Rgba::rgba(0, 0, 0, 0x80),
            chip: Rgba::rgba(0, 0, 0, 0x12),
            chip_hover: Rgba::rgba(0, 0, 0, 0x22),
            outline: Rgba::rgba(0, 0, 0, 0x1E),
            accent,
        }
    } else {
        Pal {
            bg: Rgba::rgb(0, 0, 0),
            fg: Rgba::WHITE,
            dim: Rgba::rgba(255, 255, 255, 0xCC),
            faint: Rgba::rgba(255, 255, 255, 0x8C),
            chip: Rgba::rgba(255, 255, 255, 0x1F),
            chip_hover: Rgba::rgba(255, 255, 255, 0x33),
            outline: Rgba::rgba(255, 255, 255, 0x18),
            accent,
        }
    }
}

/// Холст в логических точках острова с началом в левом верхнем углу пилюли.
pub(crate) struct P<'a> {
    pub c: Canvas<'a>,
    pub s: f32,
    pub ox: f32,
    pub oy: f32,
    pub hits: Vec<(f32, f32, f32, f32, Hit)>,
}

impl<'a> P<'a> {
    fn x(&self, v: f32) -> f32 {
        (v + self.ox) * self.s
    }
    fn y(&self, v: f32) -> f32 {
        (v + self.oy) * self.s
    }
    pub fn hit(&mut self, x: f32, y: f32, w: f32, h: f32, hit: Hit) {
        self.hits.push((x, y, w, h, hit));
    }
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Rgba) {
        let (px, py, s) = (self.x(x), self.y(y), self.s);
        self.c.fill_round_rect(px, py, w * s, h * s, r * s, color);
    }
    pub fn stroke(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32, color: Rgba) {
        let (px, py, s) = (self.x(x), self.y(y), self.s);
        self.c.stroke_round_rect(px + 0.5, py + 0.5, w * s - 1.0, h * s - 1.0, r * s, width * s, color);
    }
    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, color: Rgba) {
        let (px, py, s) = (self.x(cx), self.y(cy), self.s);
        self.c.fill_circle(px, py, r * s, color);
    }
    pub fn ring(&mut self, cx: f32, cy: f32, r: f32, width: f32, color: Rgba) {
        let (px, py, s) = (self.x(cx), self.y(cy), self.s);
        self.c.stroke_circle(px, py, r * s, width * s, color);
    }
    pub fn arc(&mut self, cx: f32, cy: f32, r: f32, start: f32, sweep: f32, width: f32, color: Rgba) {
        let (px, py, s) = (self.x(cx), self.y(cy), self.s);
        self.c.stroke_arc(px, py, r * s, start, sweep, width * s, color);
    }
    /// Текст по вертикальному центру cy, обрезанный по ширине.
    pub fn text_v(&mut self, t: &str, weight: Weight, size: f32, x: f32, cy: f32, max_w: f32, color: Rgba) -> f32 {
        let s = self.s;
        let weight = bolder(weight);
        let line = text::ellipsize(t, weight, size * s, max_w.max(1.0) * s);
        let (_, lh) = text::metrics(size * s, weight);
        let (px, py) = (self.x(x), self.y(cy) - lh / 2.0);
        self.c.draw_line(&line, px, py, color);
        line.width / s
    }
    pub fn text_c(&mut self, t: &str, weight: Weight, size: f32, x: f32, y: f32, w: f32, h: f32, color: Rgba) {
        let (px, py, s) = (self.x(x), self.y(y), self.s);
        self.c.text_centered(t, bolder(weight), size * s, px, py, w * s, h * s, color);
    }
    /// Значок пожирнее: у шрифта значков одна толщина линий, поэтому рисуем его несколько раз со сдвигом
    /// на долю точки.
    pub fn icon(&mut self, glyph: char, size: f32, cx: f32, cy: f32, color: Rgba) {
        let (px, py, s) = (self.x(cx), self.y(cy), self.s);
        let d = (size * s * 0.035).clamp(0.4, 1.1);
        for (dx, dy) in [(-d, 0.0), (d, 0.0), (0.0, -d), (0.0, d), (0.0, 0.0)] {
            self.c.icon(glyph, size * s, px + dx, py + dy, color);
        }
    }
    pub fn image(&mut self, img: &tiny_skia::Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, opacity: f32) {
        let (px, py, s) = (self.x(x), self.y(y), self.s);
        self.c.draw_image_fill(img, px, py, w * s, h * s, r * s, opacity);
    }
    pub fn poly(&mut self, pts: &[(f32, f32)], color: Rgba) {
        let pts: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| (self.x(x), self.y(y))).collect();
        self.c.fill_polygon(&pts, color);
    }
    pub fn line(&mut self, pts: &[(f32, f32)], width: f32, color: Rgba) {
        let pts: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| (self.x(x), self.y(y))).collect();
        let s = self.s;
        self.c.stroke_polyline(&pts, width * s, color);
    }
}

pub(crate) fn measure(t: &str, weight: Weight, size: f32) -> f32 {
    text::measure(t, bolder(weight), size)
}

/// Тонкого текста на острове нет: на чёрном и полупрозрачном он читается плохо.
fn bolder(weight: Weight) -> Weight {
    match weight {
        Weight::Regular => Weight::Semibold,
        w => w,
    }
}

/// Самый живой цвет обложки — для полосы, эквалайзера и строки песни.
pub(crate) fn vivid_color(pm: &tiny_skia::Pixmap) -> Rgba {
    let data = pm.data();
    let (mut best, mut sum, mut weight) = (0.0f32, [0.0f32; 3], 0.0f32);
    for px in data.chunks_exact(4).step_by(7) {
        let (b, g, r) = (px[0] as f32, px[1] as f32, px[2] as f32);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let sat = if max > 0.0 { (max - min) / max } else { 0.0 };
        let wgt = sat * sat * (max / 255.0);
        best = best.max(wgt);
        sum[0] += r * wgt;
        sum[1] += g * wgt;
        sum[2] += b * wgt;
        weight += wgt;
    }
    if weight < 1.0 || best < 0.05 {
        return crate::theme::accent();
    }
    let mut c = [sum[0] / weight, sum[1] / weight, sum[2] / weight];
    // Тёмный цвет на чёрной пилюле не видно — осветляем
    let max = c[0].max(c[1]).max(c[2]).max(1.0);
    if max < 200.0 {
        let k = 200.0 / max;
        for v in &mut c {
            *v = (*v * k).min(255.0);
        }
    }
    Rgba::rgb(c[0] as u8, c[1] as u8, c[2] as u8)
}

fn music_accent(i: &Island, pal: &Pal) -> Rgba {
    i.cover.as_ref().map(|c| c.accent).unwrap_or(pal.accent)
}

/// Строка песни, которая звучит сейчас.
pub(crate) fn current_lyric(i: &Island) -> Option<String> {
    if !i.settings.lyrics {
        return None;
    }
    let track = i.player.track.as_ref()?;
    let (key, lyrics) = i.player.lyrics.as_ref()?;
    if *key != track.key() || lyrics.status != crate::media::lyrics::Status::Synced {
        return None;
    }
    let pos = track.position_now() + Duration::from_millis(250);
    let line = lyrics.lines.iter().take_while(|l| l.time <= pos).last()?;
    let t = line.text.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Ширина текста, будто все цифры — самая широкая из них: пилюля не дёргается каждую секунду.
fn steady_width(t: &str, weight: Weight, size: f32) -> f32 {
    let widest = ('0'..='9').max_by(|a, b| measure(&a.to_string(), weight, size).total_cmp(&measure(&b.to_string(), weight, size))).unwrap_or('0');
    let wide: String = t.chars().map(|c| if c.is_ascii_digit() { widest } else { c }).collect();
    measure(&wide, weight, size)
}

pub(crate) fn collapsed_width(i: &Island) -> f32 {
    let clock = steady_width(&i.clock_text(), Weight::Semibold, 13.5);
    let mut w = 0.0;
    let gap = 8.0;
    let mut add = |x: f32| {
        if w > 0.0 {
            w += gap;
        }
        w += x;
    };
    if let Some(c) = &i.countdown {
        add(20.0 + steady_width(&format_left(c.left()), Weight::Semibold, 12.5));
    }
    if let Some(a) = i.agents.first() {
        add(11.0 + measure(agent_short(&a.name), Weight::Semibold, 11.5));
    }
    if i.settings.show_mic_cam && !i.mic_users.is_empty() {
        add(7.0);
    }
    if i.settings.show_mic_cam && !i.cam_users.is_empty() {
        add(7.0);
    }
    if i.settings.show_layout {
        add(measure(&i.layout, Weight::Semibold, 11.0) + 12.0);
    }
    if i.caps {
        add(16.0);
    }
    if i.settings.show_battery {
        if let Some(b) = i.battery {
            add(measure(&format!("{}%", b.percent), Weight::Semibold, 12.0) + 28.0);
        }
    }
    let player = if quick_track(i).is_some() { QUICK_W + 8.0 } else { 0.0 };
    (14.0 + clock + player + 22.0 + w + 14.0).max(150.0)
}

/// Ширина кнопок плеера в свёрнутой пилюле.
const QUICK_W: f32 = 3.0 * 26.0;

/// Трек для кнопок свёрнутой пилюли: любой, даже на паузе — чтобы музыку можно было включить.
pub(crate) fn quick_track(i: &Island) -> Option<&crate::media::player::Track> {
    i.player.track.as_ref().filter(|t| !t.title.trim().is_empty())
}

/// Назад, пауза или пуск, вперёд — рядком от x; круг под курсором.
fn quick_buttons(p: &mut P, i: &Island, x: f32, h: f32, playing: bool, pal: &Pal, accent: Rgba) {
    let cy = h / 2.0;
    let buttons = [
        (Hit::Prev, '\u{E892}', 10.0),
        (Hit::PlayPause, if playing { '\u{E769}' } else { '\u{E768}' }, 12.0),
        (Hit::Next, '\u{E893}', 10.0),
    ];
    for (k, (hit, glyph, size)) in buttons.into_iter().enumerate() {
        let cx = x + 13.0 + k as f32 * 26.0;
        if i.hover == hit {
            p.circle(cx, cy, 12.0, pal.chip_hover);
        }
        let color = if hit == Hit::PlayPause && playing { accent } else { pal.fg };
        p.icon(glyph, size, cx, cy, color);
        p.hit(cx - 13.0, 0.0, 26.0, h, hit);
    }
}

fn agent_short(name: &str) -> &str {
    name.split_whitespace().next().unwrap_or(name)
}

fn agent_color(status: Status) -> Rgba {
    match status {
        Status::Working => PURPLE,
        Status::Waiting => ORANGE,
        Status::Done => GREEN,
    }
}

fn pulse(speed: f32) -> f32 {
    let t = util::tick_ms() as f32 / 1000.0 * speed;
    0.55 + 0.45 * t.sin().abs()
}

/// Кадр окна: буфер, где в нём пилюля (логические точки) и общая непрозрачность.
pub(crate) fn frame(i: &mut Island) -> Option<(tiny_skia::Pixmap, f32, u8)> {
    let s = i.scale;
    let (w, h) = (i.w.value(), i.h.value());
    let mode = i.mode();
    let pal = palette(i.settings.theme);
    let lyric = match mode {
        Mode::Wide | Mode::Normal if i.track_visible().is_some() => current_lyric(i),
        _ => None,
    };
    let lyric_w = lyric.as_ref().map(|l| (measure(l, Weight::Semibold, 13.0) + 36.0).clamp(140.0, 620.0)).unwrap_or(0.0);
    let bw = w.max(lyric_w) + PAD * 2.0;
    let bh = TOP + h + PAD + if lyric.is_some() { LYRIC_H + LYRIC_GAP } else { 0.0 };
    let mut pm = gfx::pixmap((bw * s).ceil() as u32, (bh * s).ceil() as u32);
    let pill_left = (bw - w) / 2.0;
    // Автоскрытие: остров уезжает за верхний край и гаснет
    let peek = i.peek.value().clamp(0.0, 1.0);
    let slide = (bh - PAD) * (1.0 - peek);
    let hits = {
        let mut p = P { c: Canvas::new(pm.as_mut()), s, ox: pill_left, oy: TOP - slide, hits: Vec::new() };
        let radius = if h > ACTIVITY_H + 12.0 { if mode == Mode::Expanded { 30.0 } else { 24.0f32.min(h / 2.0) } } else { h / 2.0 };
        let accent = music_accent(i, &pal);

        // Край-эквалайзер: полосы выступают из-под нижнего края
        let music_on = i.track_visible().map(|t| t.playing).unwrap_or(false);
        if i.settings.equalizer && music_on && matches!(mode, Mode::Wide | Mode::Mini | Mode::Normal | Mode::Expanded) {
            let span = (w - 2.0 * radius.min(w / 3.0)).max(40.0);
            let reach = if mode == Mode::Expanded { 9.0 } else { 7.0 };
            draw_eq(&mut p, i.settings.eq_style, &crate::eq::levels(), (w - span) / 2.0, span, h, reach, accent);
        }

        p.rect(-1.0, -1.0, w + 2.0, h + 2.0, radius + 1.0, pal.outline);
        p.rect(0.0, 0.0, w, h, radius, pal.bg);

        let (tw, th) = i.target_size();
        let near = (w - tw).abs() < 40.0 && (h - th).abs() < 12.0;
        p.c.alpha = i.content.value();
        if near || !i.w.active() {
            match mode {
                Mode::Expanded => draw_panel(&mut p, i, &pal),
                Mode::Card => {
                    if let Some(card) = i.cards.front() {
                        draw_card(&mut p, i, card, w, h, &pal);
                    }
                }
                Mode::Activity => {
                    if let Some((a, _)) = &i.activity {
                        draw_activity(&mut p, i, a, w, &pal);
                    }
                }
                Mode::Recording => draw_status(&mut p, w, &pal, true),
                Mode::Voice => draw_status(&mut p, w, &pal, false),
                Mode::Wide => draw_wide(&mut p, i, w, h, &pal, accent),
                Mode::Mini => draw_mini(&mut p, i, w, h, &pal, accent),
                Mode::Normal => draw_normal(&mut p, i, w, &pal),
            }
        }
        p.c.alpha = 1.0;
        if let Some(l) = &lyric {
            let y = h + LYRIC_GAP;
            let x = (w - lyric_w) / 2.0;
            p.rect(x - 1.0, y - 1.0, lyric_w + 2.0, LYRIC_H + 2.0, LYRIC_H / 2.0 + 1.0, pal.outline);
            p.rect(x, y, lyric_w, LYRIC_H, LYRIC_H / 2.0, pal.bg);
            p.text_v(l, Weight::Semibold, 13.0, x + 18.0, y + LYRIC_H / 2.0, lyric_w - 36.0, accent);
        }
        p.hits
    };
    i.hits = hits;
    let e = i.expand.value();
    let op = i.settings.opacity_collapsed as f32 + (i.settings.opacity_expanded - i.settings.opacity_collapsed) as f32 * e;
    let alpha = (op.clamp(20.0, 100.0) * 2.55 * peek).round() as u8;
    Some((pm, pill_left, alpha))
}

/// Цвет по оттенку (0–1) для радужного эквалайзера.
fn hue(t: f32) -> Rgba {
    let h = (t.fract() * 6.0).max(0.0);
    let x = 1.0 - (h % 2.0 - 1.0).abs();
    let (r, g, b) = match h as u32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    // Пастельнее, чтобы не резало глаз на чёрном
    let f = |v: f32| (90.0 + v * 165.0) as u8;
    Rgba::rgb(f(r), f(g), f(b))
}

/// Край-эквалайзер: выступает из-под нижнего края пилюли между x0 и x0 + span.
fn draw_eq(p: &mut P, style: EqStyle, levels: &[f32], x0: f32, span: f32, h: f32, reach: f32, accent: Rgba) {
    let n = levels.len();
    if n < 2 {
        return;
    }
    let at = |k: usize| x0 + span * k as f32 / (n - 1) as f32;
    match style {
        EqStyle::Bars | EqStyle::Rainbow => {
            for (k, lv) in levels.iter().enumerate() {
                if *lv < 0.04 {
                    continue;
                }
                let color = if style == EqStyle::Rainbow { hue(k as f32 / n as f32 * 0.85) } else { accent };
                let bar = 1.5 + lv * reach;
                p.rect(at(k) - 3.5, h - 4.0, 7.0, bar + 6.0, 3.5, color.with_alpha(0.22));
                p.rect(at(k) - 1.5, h - 4.0, 3.0, bar + 4.0, 1.5, color.with_alpha(0.9));
            }
        }
        EqStyle::Mirror => {
            // Низкие в центре, высокие расходятся к краям
            let half = span / 2.0;
            let cx = x0 + half;
            for (k, lv) in levels.iter().enumerate() {
                if *lv < 0.04 {
                    continue;
                }
                let d = half * (k as f32 + 0.5) / n as f32;
                let bar = 1.5 + lv * reach;
                for x in [cx - d, cx + d] {
                    p.rect(x - 1.25, h - 4.0, 2.5, bar + 4.0, 1.25, accent.with_alpha(0.55 + 0.45 * lv));
                }
            }
        }
        EqStyle::Wave => {
            let mut pts: Vec<(f32, f32)> = Vec::with_capacity(n + 2);
            pts.push((x0 - 6.0, h - 1.0));
            for (k, lv) in levels.iter().enumerate() {
                pts.push((at(k), h + lv * reach * 1.2));
            }
            pts.push((x0 + span + 6.0, h - 1.0));
            p.poly(&pts, accent.with_alpha(0.3));
            p.line(&pts[1..pts.len() - 1], 2.0, accent.with_alpha(0.95));
        }
        EqStyle::Dots => {
            for (k, lv) in levels.iter().enumerate() {
                let dots = (lv * 4.0).round() as usize;
                for d in 0..dots {
                    p.circle(at(k), h + 2.5 + d as f32 * 4.0, 1.4, accent.with_alpha(1.0 - d as f32 * 0.2));
                }
            }
        }
        EqStyle::Glow => {
            let level = levels.iter().copied().fold(0.0f32, f32::max).max(levels.iter().sum::<f32>() / n as f32);
            if level < 0.03 {
                return;
            }
            let grow = 0.35 + 0.65 * level;
            let w = span * grow;
            let x = x0 + (span - w) / 2.0;
            p.rect(x - 6.0, h - 3.0, w + 12.0, 4.0 + level * 7.0, 5.0, accent.with_alpha(0.15));
            p.rect(x, h - 2.0, w, 2.0 + level * 3.0, 2.5, accent.with_alpha(0.35 + 0.6 * level));
        }
    }
}

// ---------- Свёрнутая пилюля ----------

fn draw_normal(p: &mut P, i: &Island, w: f32, pal: &Pal) {
    let h = COLLAPSED_H;
    let cy = h / 2.0;
    p.text_v(&i.clock_text(), Weight::Semibold, 13.5, 14.0, cy, 120.0, pal.fg);
    let cw = steady_width(&i.clock_text(), Weight::Semibold, 13.5);
    if let Some(track) = quick_track(i) {
        let accent = music_accent(i, pal);
        p.rect(14.0 + cw + 10.0, cy - 8.0, 1.0, 16.0, 0.5, pal.outline);
        quick_buttons(p, i, 14.0 + cw + 12.0, h, track.playing, pal, accent);
    }
    let mut x = w - 14.0;
    let gap = 8.0;
    if i.settings.show_battery {
        if let Some(b) = i.battery {
            x -= 24.0;
            draw_battery(p, x, cy, b, pal);
            let label = format!("{}%", b.percent);
            let lw = measure(&label, Weight::Semibold, 12.0);
            x -= 4.0 + lw;
            p.text_v(&label, Weight::Semibold, 12.0, x, cy, lw + 2.0, pal.fg);
            x -= gap;
        }
    }
    if i.caps {
        x -= 16.0;
        p.text_c("⇪", Weight::Semibold, 15.0, x, 0.0, 16.0, h, pal.fg);
        x -= gap;
    }
    if i.settings.show_layout {
        let lw = measure(&i.layout, Weight::Semibold, 11.0) + 12.0;
        x -= lw;
        p.rect(x, cy - 9.0, lw, 18.0, 6.0, pal.chip);
        p.text_c(&i.layout, Weight::Semibold, 11.0, x, cy - 9.0, lw, 18.0, pal.fg);
        x -= gap;
    }
    if i.settings.show_mic_cam && !i.cam_users.is_empty() {
        x -= 7.0;
        p.circle(x + 3.5, cy, 3.5, GREEN);
        x -= gap;
    }
    if i.settings.show_mic_cam && !i.mic_users.is_empty() {
        x -= 7.0;
        p.circle(x + 3.5, cy, 3.5, ORANGE);
        x -= gap;
    }
    if let Some(a) = i.agents.first() {
        let name = agent_short(&a.name);
        let lw = measure(name, Weight::Semibold, 11.5);
        x -= lw;
        p.text_v(name, Weight::Semibold, 11.5, x, cy, lw + 2.0, pal.dim);
        x -= 11.0;
        let alpha = if a.status == Status::Working { pulse(3.0) } else { 1.0 };
        p.circle(x + 3.5, cy, 3.5, agent_color(a.status).with_alpha(alpha));
        x -= gap;
    }
    if let Some(cd) = &i.countdown {
        let label = format_left(cd.left());
        let lw = measure(&label, Weight::Semibold, 12.5);
        x -= lw;
        let color = if cd.paused.is_some() { pal.dim } else { ORANGE };
        p.text_v(&label, Weight::Semibold, 12.5, x, cy, lw + 2.0, color);
        x -= 20.0;
        p.icon('\u{E916}', 13.0, x + 8.0, cy, color);
    }
}

fn draw_status(p: &mut P, w: f32, pal: &Pal, recording: bool) {
    let cy = COLLAPSED_H / 2.0;
    let (since, label, stop) = if recording {
        (crate::tools::record::started(), tr("Запись", "Recording"), Hit::RecordStop)
    } else {
        (crate::voice::listening_since(), tr("Слушаю…", "Listening…"), Hit::VoiceStop)
    };
    let a = pulse(2.5);
    if recording {
        p.circle(20.0, cy, 5.0, RED.with_alpha(a));
    } else {
        p.icon('\u{E720}', 14.0, 20.0, cy, RED.with_alpha(a));
    }
    let lw = p.text_v(label, Weight::Semibold, 13.0, 34.0, cy, 110.0, pal.fg);
    let elapsed = since.map(|t| t.elapsed()).unwrap_or_default();
    p.text_v(&format_time(elapsed), Weight::Semibold, 13.0, 42.0 + lw, cy, 70.0, pal.dim);
    p.rect(w - 50.0, cy - 12.0, 40.0, 24.0, 12.0, RED.with_alpha(0.25));
    p.rect(w - 34.5, cy - 4.5, 9.0, 9.0, 2.0, RED);
    p.hit(w - 54.0, 0.0, 54.0, COLLAPSED_H, stop);
}

fn progress_of(track: &crate::media::player::Track) -> f32 {
    if track.duration.is_zero() {
        0.0
    } else {
        (track.position_now().as_secs_f32() / track.duration.as_secs_f32()).clamp(0.0, 1.0)
    }
}

fn draw_wide(p: &mut P, i: &Island, w: f32, h: f32, pal: &Pal, accent: Rgba) {
    let Some(track) = i.track_visible() else { return };
    let cy = h / 2.0 - 1.0;
    // Четыре полоски — живой значок музыки
    let levels = if i.settings.equalizer && track.playing { crate::eq::levels() } else { [0.0; crate::eq::BANDS] };
    for k in 0..4 {
        let lv = if track.playing {
            levels[2 + k * 5].max(0.15)
        } else {
            0.2
        };
        let bh = 4.0 + lv * 10.0;
        p.rect(14.0 + k as f32 * 4.5, cy - bh / 2.0, 3.0, bh, 1.5, accent);
    }
    let name = if track.artist.trim().is_empty() { track.title.clone() } else { format!("{} — {}", track.title, track.artist) };
    let time = format_time(track.position_now());
    let tw = measure(&time, Weight::Semibold, 11.5);
    let bx = w - 8.0 - QUICK_W;
    p.text_v(&name, Weight::Semibold, 12.5, 38.0, cy, bx - 38.0 - tw - 16.0, pal.fg);
    p.text_v(&time, Weight::Semibold, 11.5, bx - 6.0 - tw, cy, tw + 2.0, pal.dim);
    quick_buttons(p, i, bx, h - 2.0, track.playing, pal, accent);
    let (x0, x1) = (18.0, w - 18.0);
    p.rect(x0, h - 4.0, x1 - x0, 2.0, 1.0, pal.chip);
    p.rect(x0, h - 4.0, (x1 - x0) * progress_of(track), 2.0, 1.0, accent);
}

fn draw_mini(p: &mut P, i: &Island, w: f32, h: f32, pal: &Pal, accent: Rgba) {
    let Some(track) = i.track_visible() else { return };
    match &i.cover {
        Some(c) => p.image(&c.image, 8.0, 8.0, 36.0, 36.0, 9.0, 1.0),
        None => {
            p.rect(8.0, 8.0, 36.0, 36.0, 9.0, pal.chip);
            p.icon('\u{E8D6}', 15.0, 26.0, 26.0, pal.dim);
        }
    }
    let text_w = w - 54.0 - 116.0;
    p.text_v(&track.title, Weight::Semibold, 13.0, 54.0, 17.0, text_w, pal.fg);
    match current_lyric(i) {
        Some(l) => p.text_v(&l, Weight::Semibold, 11.5, 54.0, 33.0, text_w, accent),
        None => p.text_v(&track.artist, Weight::Regular, 11.5, 54.0, 33.0, text_w, pal.dim),
    };
    p.rect(54.0, 43.0, text_w, 2.0, 1.0, pal.chip);
    p.rect(54.0, 43.0, text_w * progress_of(track), 2.0, 1.0, accent);
    let buttons = [
        (Hit::Prev, '\u{E892}', w - 96.0),
        (Hit::PlayPause, if track.playing { '\u{E769}' } else { '\u{E768}' }, w - 62.0),
        (Hit::Next, '\u{E893}', w - 28.0),
    ];
    for (hit, glyph, cx) in buttons {
        if i.hover == hit {
            p.circle(cx, h / 2.0, 15.0, pal.chip_hover);
        }
        p.icon(glyph, if hit == Hit::PlayPause { 15.0 } else { 13.0 }, cx, h / 2.0, pal.fg);
        p.hit(cx - 16.0, 0.0, 32.0, h, hit);
    }
}

fn draw_activity(p: &mut P, i: &Island, activity: &Activity, w: f32, pal: &Pal) {
    let h = ACTIVITY_H;
    let cy = h / 2.0;
    match activity {
        Activity::Volume => {
            let snap = audio::snapshot();
            let m = &snap.master;
            p.icon(audio::volume_glyph(m.volume, m.muted), 16.0, 26.0, cy, pal.fg);
            let (x0, x1) = (48.0, w - 58.0);
            p.rect(x0, cy - 3.0, x1 - x0, 6.0, 3.0, pal.chip_hover);
            p.rect(x0, cy - 3.0, (x1 - x0) * m.volume / 100.0, 6.0, 3.0, if m.muted { pal.dim } else { pal.fg });
            let label = if m.muted { "—".to_string() } else { format!("{}", m.volume as i32) };
            let lw = measure(&label, Weight::Semibold, 13.0);
            p.text_v(&label, Weight::Semibold, 13.0, w - 18.0 - lw, cy, lw + 2.0, pal.fg);
        }
        Activity::Layout(layout) => {
            p.icon('\u{E765}', 16.0, 26.0, cy, pal.fg);
            p.text_v(tr("Раскладка", "Layout"), Weight::Semibold, 13.0, 48.0, cy, 120.0, pal.fg);
            let lw = measure(layout, Weight::Bold, 14.0) + 16.0;
            p.rect(w - 16.0 - lw, cy - 12.0, lw, 24.0, 8.0, pal.chip_hover);
            p.text_c(layout, Weight::Bold, 14.0, w - 16.0 - lw, cy - 12.0, lw, 24.0, pal.fg);
        }
        Activity::Caps(on) => {
            p.text_c("⇪", Weight::Semibold, 18.0, 14.0, 0.0, 24.0, h, pal.fg);
            p.text_v("Caps Lock", Weight::Semibold, 13.0, 48.0, cy, 120.0, pal.fg);
            let (label, color) = if *on { (tr("ВКЛ", "ON"), GREEN) } else { (tr("ВЫКЛ", "OFF"), pal.dim) };
            let lw = measure(label, Weight::Bold, 12.5);
            p.text_v(label, Weight::Bold, 12.5, w - 18.0 - lw, cy, lw + 2.0, color);
        }
        Activity::Mic(muted) => {
            draw_mic(p, 26.0, cy, 16.0, *muted, if *muted { RED } else { pal.fg });
            let label = if *muted { tr("Микрофон выключен", "Microphone off") } else { tr("Микрофон включён", "Microphone on") };
            p.text_v(label, Weight::Semibold, 13.0, 48.0, cy, w - 60.0, pal.fg);
        }
        Activity::MicUse(name) | Activity::CameraUse(name) => {
            let mic = matches!(activity, Activity::MicUse(_));
            let color = if mic { ORANGE } else { GREEN };
            if mic {
                draw_mic(p, 26.0, cy, 16.0, false, color);
            } else {
                p.icon('\u{E714}', 16.0, 26.0, cy, color);
            }
            let label = if mic { tr("Микрофон", "Microphone") } else { tr("Камера", "Camera") };
            let lw = p.text_v(label, Weight::Regular, 13.0, 48.0, cy, 100.0, pal.dim);
            p.text_v(name, Weight::Semibold, 13.0, 56.0 + lw, cy, w - 72.0 - lw, pal.fg);
        }
        Activity::Power(percent, plugged) => {
            let b = Battery { percent: *percent, charging: *plugged, plugged: *plugged };
            draw_battery(p, 14.0, cy, b, pal);
            let label = if *plugged { tr("Зарядка", "Charging") } else { tr("От батареи", "On battery") };
            p.text_v(label, Weight::Semibold, 13.0, 48.0, cy, 140.0, pal.fg);
            let t = format!("{percent}%");
            let lw = measure(&t, Weight::Bold, 13.0);
            p.text_v(&t, Weight::Bold, 13.0, w - 18.0 - lw, cy, lw + 2.0, if *plugged { GREEN } else { pal.fg });
        }
    }
    let _ = i;
}

// ---------- Карточки ----------

fn card_text_x(card: &Card) -> f32 {
    if card.thumb.is_some() {
        16.0 + 120.0 + 14.0
    } else {
        64.0
    }
}

/// Текст карточки по строкам: не больше четырёх, последняя обрезается многоточием при отрисовке.
fn body_lines(card: &Card) -> Vec<String> {
    let body = card.body.trim();
    if body.is_empty() {
        return Vec::new();
    }
    let width = CARD_W - card_text_x(card) - 18.0;
    let mut lines: Vec<String> = Vec::new();
    for paragraph in body.split('\n') {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if current.is_empty() { word.to_string() } else { format!("{current} {word}") };
            if measure(&candidate, Weight::Regular, 12.5) <= width || current.is_empty() {
                current = candidate;
            } else {
                lines.push(std::mem::replace(&mut current, word.to_string()));
            }
        }
        lines.push(current);
        if lines.len() > 4 {
            break;
        }
    }
    if lines.len() > 4 {
        lines.truncate(4);
        lines[3].push_str(" …");
    }
    lines
}

pub(crate) fn card_height(card: &Card) -> f32 {
    let lines = body_lines(card).len() as f32;
    let mut h = 16.0 + 22.0 + lines * 18.0 + 14.0;
    if card.thumb.is_some() {
        h = h.max(16.0 + 72.0 + 14.0);
    } else {
        h = h.max(16.0 + 36.0 + 14.0);
    }
    if !card.buttons.is_empty() {
        h += 38.0;
    }
    h
}

fn draw_card(p: &mut P, i: &Island, card: &Card, w: f32, h: f32, pal: &Pal) {
    // Иконка или миниатюра
    if let Some(thumb) = &card.thumb {
        p.image(thumb, 16.0, 16.0, 120.0, 72.0, 10.0, 1.0);
        p.stroke(16.0, 16.0, 120.0, 72.0, 10.0, 1.0, pal.outline);
    } else if let Some(sw) = card.swatch {
        p.circle(34.0, 34.0, 18.0, sw);
        p.ring(34.0, 34.0, 18.0, 1.0, pal.outline);
    } else if card.busy {
        p.circle(34.0, 34.0, 18.0, card.tint.with_alpha(0.18));
        let t = util::tick_ms() as f32 / 1000.0 * 5.0;
        p.arc(34.0, 34.0, 9.0, t, 4.0, 2.5, card.tint);
    } else {
        p.circle(34.0, 34.0, 18.0, card.tint.with_alpha(0.2));
        p.icon(card.glyph, 15.0, 34.0, 34.0, card.tint);
    }
    let x = card_text_x(card);
    let close = card.sticky || card.agent.is_some();
    let title_w = w - x - if close { 44.0 } else { 18.0 };
    p.text_v(&card.title, Weight::Semibold, 14.0, x, 27.0, title_w, pal.fg);
    for (k, line) in body_lines(card).iter().enumerate() {
        p.text_v(line, Weight::Regular, 12.5, x, 47.0 + k as f32 * 18.0, w - x - 18.0, pal.dim);
    }
    if close {
        let hover = i.hover == Hit::CardClose;
        if hover {
            p.circle(w - 26.0, 26.0, 12.0, pal.chip_hover);
        }
        p.icon('\u{E711}', 10.0, w - 26.0, 26.0, pal.dim);
        p.hit(w - 40.0, 12.0, 28.0, 28.0, Hit::CardClose);
    }
    // Кнопки справа внизу, последняя — главная
    let mut bx = w - 16.0;
    let by = h - 16.0 - 30.0;
    let n = card.buttons.len();
    for (k, (label, _)) in card.buttons.iter().enumerate().rev() {
        let bw = measure(label, Weight::Semibold, 12.5) + 28.0;
        bx -= bw;
        let hit = Hit::CardButton(k);
        let primary = k == n - 1;
        let fill = if primary { card.tint } else if i.hover == hit { pal.chip_hover } else { pal.chip };
        p.rect(bx, by, bw, 30.0, 15.0, if primary && i.hover == hit { fill.with_alpha(0.85) } else { fill });
        let color = if primary { contrast(card.tint) } else { pal.fg };
        p.text_c(label, Weight::Semibold, 12.5, bx, by, bw, 30.0, color);
        p.hit(bx, by, bw, 30.0, hit);
        bx -= 8.0;
    }
}

/// Чёрный или белый текст на цветной кнопке.
fn contrast(c: Rgba) -> Rgba {
    let l = 0.299 * c.r as f32 + 0.587 * c.g as f32 + 0.114 * c.b as f32;
    if l > 160.0 {
        Rgba::BLACK
    } else {
        Rgba::WHITE
    }
}

// ---------- Раскрытая панель ----------

fn date_text() -> String {
    use chrono::Datelike;
    let now = chrono::Local::now();
    if crate::i18n::is_english() {
        return now.format("%A, %B %-d").to_string();
    }
    const DAYS: [&str; 7] = ["понедельник", "вторник", "среда", "четверг", "пятница", "суббота", "воскресенье"];
    const MONTHS: [&str; 12] = ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];
    format!("{}, {} {}", DAYS[now.weekday().num_days_from_monday() as usize], now.day(), MONTHS[now.month0() as usize])
}

/// Блок панели: тонкая рамка и едва заметный оттенок — вид OmniFlex вместо сплошных серых плиток.
fn block(p: &mut P, x: f32, y: f32, w: f32, h: f32, r: f32, pal: &Pal, tint: Option<Rgba>) {
    p.rect(x, y, w, h, r, pal.outline);
    p.rect(x + 1.0, y + 1.0, w - 2.0, h - 2.0, r - 1.0, pal.bg);
    if let Some(t) = tint {
        p.rect(x + 1.0, y + 1.0, w - 2.0, h - 2.0, r - 1.0, t);
    }
}

/// Подпись раздела: мелкие прописные буквы.
fn caption(p: &mut P, text: &str, x: f32, cy: f32, pal: &Pal) -> f32 {
    p.text_v(&text.to_uppercase(), Weight::Bold, 10.5, x, cy, 220.0, pal.faint)
}

/// Круглая кнопка со значком.
fn round_button(p: &mut P, i: &Island, pal: &Pal, glyph: char, cx: f32, cy: f32, r: f32, hit: Hit, fill: Option<Rgba>, color: Rgba) {
    match fill {
        Some(f) => p.circle(cx, cy, r, f),
        None if i.hover == hit => p.circle(cx, cy, r, pal.chip_hover),
        None => p.ring(cx, cy, r - 0.5, 1.0, pal.outline),
    }
    p.icon(glyph, r * 0.62, cx, cy, color);
    p.hit(cx - r, cy - r, r * 2.0, r * 2.0, hit);
}

fn draw_panel(p: &mut P, i: &Island, pal: &Pal) {
    let w = EXP_W;
    // Слева: крупные часы, рядом дата и батарея
    let clock = i.clock_text();
    let cw = p.text_v(&clock, Weight::Bold, 24.0, 22.0, 32.0, 160.0, pal.fg);
    let dx = 22.0 + cw + 14.0;
    p.text_v(&date_text(), Weight::Semibold, 12.0, dx, 24.0, 200.0, pal.dim);
    if let Some(b) = i.battery.filter(|_| i.settings.show_battery) {
        draw_battery(p, dx, 41.0, b, pal);
        p.text_v(&format!("{}%", b.percent), Weight::Semibold, 11.0, dx + 30.0, 41.0, 50.0, pal.faint);
    }

    // Справа: настройки, голос, вкладки сегментами
    let cy = 32.0;
    round_button(p, i, pal, '\u{E713}', w - 34.0, cy, 16.0, Hit::Settings, None, pal.dim);
    let listening = crate::voice::listening_since().is_some();
    let voice_fill = if listening { Some(RED.with_alpha(0.3)) } else { None };
    round_button(p, i, pal, '\u{E720}', w - 72.0, cy, 16.0, Hit::Voice, voice_fill, if listening { RED } else { pal.fg });

    let tabs = [(tr("Пульт", "Deck"), 0u8), (tr("Время", "Time"), 1), (tr("Лента", "Feed"), 2)];
    let widths: Vec<f32> = tabs.iter().map(|(l, _)| measure(l, Weight::Semibold, 12.5) + 30.0).collect();
    let total: f32 = widths.iter().sum::<f32>() + 8.0;
    let mut x = w - 100.0 - total;
    p.rect(x, cy - 17.0, total, 34.0, 17.0, pal.outline);
    p.rect(x + 1.0, cy - 16.0, total - 2.0, 32.0, 16.0, pal.bg);
    x += 4.0;
    for ((label, tab), tw) in tabs.iter().zip(widths) {
        let hit = Hit::Tab(*tab);
        let selected = i.tab == *tab;
        if selected {
            p.rect(x, cy - 13.0, tw, 26.0, 13.0, pal.fg);
        } else if i.hover == hit {
            p.rect(x, cy - 13.0, tw, 26.0, 13.0, pal.chip);
        }
        p.text_c(label, Weight::Semibold, 12.5, x, cy - 13.0, tw, 26.0, if selected { pal.bg } else { pal.dim });
        p.hit(x, cy - 13.0, tw, 26.0, hit);
        if *tab == 2 && i.agents.iter().any(|a| a.status == Status::Waiting) {
            p.circle(x + tw - 9.0, cy - 7.0, 3.5, ORANGE);
        }
        x += tw;
    }

    match i.tab {
        1 => draw_timer_tab(p, i, pal),
        2 => draw_feed_tab(p, i, pal),
        _ => draw_home(p, i, pal),
    }
}

fn draw_home(p: &mut P, i: &Island, pal: &Pal) {
    draw_tools(p, i, pal, 16.0, 66.0, 236.0, 206.0);
    draw_player(p, i, pal, 268.0, 66.0, 436.0, 144.0);
    draw_volume(p, i, pal, 268.0, 222.0, 436.0, 48.0);
    p.rect(16.0, 281.0, EXP_W - 32.0, 1.0, 0.5, pal.outline);
    draw_launcher(p, i, pal, 16.0, 288.0, EXP_W - 32.0, 44.0);
}

fn draw_player(p: &mut P, i: &Island, pal: &Pal, x: f32, y: f32, w: f32, h: f32) {
    let accent = music_accent(i, pal);
    let track = i.player.track.as_ref().filter(|t| !t.title.trim().is_empty());
    caption(p, tr("Сейчас играет", "Now playing"), x + 4.0, y + 8.0, pal);
    if let Some(t) = track {
        // Плеер — маленькой подписью справа от заголовка
        let lw = measure(&t.app, Weight::Semibold, 11.0).min(160.0);
        let px = x + w - lw - 22.0;
        if i.hover == Hit::Player {
            p.rect(px - 8.0, y - 2.0, lw + 30.0, 20.0, 10.0, pal.chip);
        }
        p.text_v(&t.app, Weight::Semibold, 11.0, px, y + 8.0, lw, pal.dim);
        p.icon('\u{E70D}', 7.0, px + lw + 10.0, y + 9.0, pal.faint);
        p.hit(px - 8.0, y - 2.0, lw + 30.0, 20.0, Hit::Player);
    }
    let (y, h) = (y + 22.0, h - 22.0);
    block(p, x, y, w, h, 22.0, pal, Some(accent.with_alpha(0.07)));
    let Some(track) = track else {
        p.icon('\u{E8D6}', 20.0, x + w / 2.0, y + 38.0, pal.faint);
        p.text_c(tr("Ничего не играет", "Nothing is playing"), Weight::Semibold, 13.0, x, y + 52.0, w, 22.0, pal.dim);
        if !i.player.players.is_empty() {
            chip_button(p, i, pal, tr("Выбрать плеер", "Choose player"), x + w / 2.0, y + 96.0, Hit::Player, true);
        }
        return;
    };
    // Обложка — пластинкой
    let (dcx, dcy, dr) = (x + 16.0 + 48.0, y + h / 2.0, 48.0);
    match &i.cover {
        Some(c) => p.image(&c.image, dcx - dr, dcy - dr, dr * 2.0, dr * 2.0, dr, 1.0),
        None => {
            p.circle(dcx, dcy, dr, pal.chip);
            p.icon('\u{E8D6}', 22.0, dcx, dcy, pal.dim);
        }
    }
    p.ring(dcx, dcy, dr - 0.5, 1.0, pal.outline);
    if i.cover.is_some() {
        p.circle(dcx, dcy, 7.0, pal.bg);
        p.ring(dcx, dcy, 7.0, 1.5, accent.with_alpha(0.8));
    }
    let tx = x + 132.0;
    let tw = x + w - 18.0 - tx;
    p.text_v(&track.title, Weight::Bold, 15.0, tx, y + 24.0, tw, pal.fg);
    match current_lyric(i) {
        Some(l) => p.text_v(&l, Weight::Semibold, 12.0, tx, y + 44.0, tw, accent),
        None => p.text_v(&track.artist, Weight::Regular, 12.0, tx, y + 44.0, tw, pal.dim),
    };
    // Полоса времени и время по краям под ней
    let by = y + 62.0;
    let frac = if i.dragging_seek() { i.drag_value } else { progress_of(track) };
    p.rect(tx, by, tw, 3.0, 1.5, pal.chip_hover);
    p.rect(tx, by, tw * frac, 3.0, 1.5, accent);
    if i.hover == Hit::Seek || i.dragging_seek() {
        p.circle(tx + tw * frac, by + 1.5, 5.5, accent);
    }
    p.hit(tx - 4.0, by - 8.0, tw + 8.0, 19.0, Hit::Seek);
    let pos = format_time(track.position_now());
    let dur = format_time(track.duration);
    let dw = measure(&dur, Weight::Regular, 10.5);
    p.text_v(&pos, Weight::Regular, 10.5, tx, by + 13.0, 60.0, pal.faint);
    p.text_v(&dur, Weight::Regular, 10.5, tx + tw - dw, by + 13.0, dw + 2.0, pal.faint);
    // Кнопки слева под текстом: назад, широкая «пуск» цвета обложки, вперёд
    let cy = y + h - 22.0;
    let prev_x = tx + 14.0;
    if i.hover == Hit::Prev {
        p.circle(prev_x, cy, 15.0, pal.chip_hover);
    }
    p.icon('\u{E892}', 13.0, prev_x, cy, pal.fg);
    p.hit(prev_x - 16.0, cy - 16.0, 32.0, 32.0, Hit::Prev);
    let (pw, ph) = (56.0, 30.0);
    let px = prev_x + 24.0;
    p.rect(px, cy - ph / 2.0, pw, ph, ph / 2.0, if i.hover == Hit::PlayPause { accent.with_alpha(0.85) } else { accent });
    p.icon(if track.playing { '\u{E769}' } else { '\u{E768}' }, 14.0, px + pw / 2.0, cy, contrast(accent));
    p.hit(px, cy - ph / 2.0, pw, ph, Hit::PlayPause);
    let next_x = px + pw + 24.0;
    if i.hover == Hit::Next {
        p.circle(next_x, cy, 15.0, pal.chip_hover);
    }
    p.icon('\u{E893}', 13.0, next_x, cy, pal.fg);
    p.hit(next_x - 16.0, cy - 16.0, 32.0, 32.0, Hit::Next);
}

/// Кнопка-«таблетка» с подписью и стрелкой вниз, по центру cx.
fn chip_button(p: &mut P, i: &Island, pal: &Pal, label: &str, cx: f32, cy: f32, hit: Hit, strong: bool) {
    let size = if strong { 12.5 } else { 11.5 };
    let lw = measure(label, Weight::Semibold, size).min(180.0);
    let bw = lw + 30.0;
    let bh = if strong { 28.0 } else { 22.0 };
    let x = cx - bw / 2.0;
    p.rect(x, cy - bh / 2.0, bw, bh, bh / 2.0, if i.hover == hit { pal.chip_hover } else { pal.chip });
    p.text_v(label, Weight::Semibold, size, x + 10.0, cy, lw, pal.fg);
    p.icon('\u{E70D}', 8.0, x + bw - 12.0, cy + 1.0, pal.dim);
    p.hit(x, cy - bh / 2.0, bw, bh, hit);
}

fn draw_volume(p: &mut P, i: &Island, pal: &Pal, x: f32, y: f32, w: f32, h: f32) {
    block(p, x, y, w, h, h / 2.0, pal, None);
    let snap = audio::snapshot();
    let m = &snap.master;
    let cy = y + h / 2.0;
    if i.hover == Hit::Mute {
        p.circle(x + 24.0, cy, 15.0, pal.chip_hover);
    }
    p.icon(audio::volume_glyph(m.volume, m.muted), 14.0, x + 24.0, cy, if m.muted { pal.dim } else { pal.fg });
    p.hit(x + 8.0, y, 32.0, h, Hit::Mute);
    // Толстая капсула, число — внутри
    let (sx, sw, sh) = (x + 46.0, 176.0, 24.0);
    let volume = if i.dragging_volume() { i.drag_value } else { m.volume };
    p.rect(sx, cy - sh / 2.0, sw, sh, sh / 2.0, pal.chip);
    let fill = (sw * volume / 100.0).max(sh);
    p.rect(sx, cy - sh / 2.0, fill, sh, sh / 2.0, if m.muted { pal.dim } else { pal.fg });
    let label = format!("{}", volume as i32);
    // Число внутри заливки, а пока она короткая — сразу за ней
    if fill > 40.0 {
        p.text_v(&label, Weight::Bold, 11.5, sx + 11.0, cy, 30.0, if m.muted { pal.fg } else { pal.bg });
    } else {
        p.text_v(&label, Weight::Bold, 11.5, sx + fill + 8.0, cy, 30.0, pal.fg);
    }
    p.hit(sx - 4.0, y, sw + 8.0, h, Hit::Volume);
    // Микрофон
    let mx = sx + sw + 24.0;
    let mic_fill = if snap.mic_muted { Some(RED.with_alpha(0.25)) } else { None };
    let mic_color = if snap.mic_muted { RED } else if snap.mic_available { pal.fg } else { pal.dim };
    match mic_fill {
        Some(f) => p.circle(mx, cy, 15.0, f),
        None if i.hover == Hit::Mic => p.circle(mx, cy, 15.0, pal.chip_hover),
        None => p.ring(mx, cy, 14.5, 1.0, pal.outline),
    }
    draw_mic(p, mx, cy, 12.0, snap.mic_muted || !snap.mic_available, mic_color);
    p.hit(mx - 16.0, y, 32.0, h, Hit::Mic);
    // Устройство — текстом со стрелкой
    let dx = mx + 26.0;
    let dw = x + w - 12.0 - dx;
    if i.hover == Hit::Device {
        p.rect(dx, cy - 14.0, dw, 28.0, 14.0, pal.chip);
    }
    let dev = snap.devices.iter().find(|d| d.id == snap.device_id).map(|d| d.glyph()).unwrap_or('\u{E7F5}');
    p.icon(dev, 11.0, dx + 14.0, cy, pal.dim);
    p.text_v(&snap.device_name, Weight::Semibold, 11.5, dx + 28.0, cy, dw - 46.0, pal.fg);
    p.icon('\u{E70D}', 8.0, dx + dw - 12.0, cy + 1.0, pal.dim);
    p.hit(dx, y, dw, h, Hit::Device);
}

fn draw_tools(p: &mut P, i: &Island, pal: &Pal, x: f32, y: f32, w: f32, h: f32) {
    let recording = crate::tools::record::is_recording();
    caption(p, tr("Инструменты", "Tools"), x + 4.0, y + 8.0, pal);
    // Последние цвета пипетки — у подписи, щелчок копирует
    let colors = settings::read(|s| s.tools.colors.clone());
    for (ci, hex) in colors.iter().take(4).enumerate() {
        let Some(c) = parse_hex(hex) else { continue };
        let cx = x + w - 8.0 - ci as f32 * 16.0;
        p.circle(cx, y + 8.0, 6.0, c);
        p.ring(cx, y + 8.0, 6.0, 1.0, pal.outline);
        p.hit(cx - 8.0, y - 2.0, 16.0, 20.0, Hit::Color(ci));
    }
    let tools = [
        (Tool::Region, '\u{E7A8}', tr("Область", "Region")),
        (Tool::Full, '\u{E7F4}', tr("Экран", "Screen")),
        (Tool::Text, '\u{E8D2}', tr("Текст", "Text")),
        (Tool::Qr, '\u{ED14}', tr("QR", "QR")),
        (Tool::Record, '\u{E714}', if recording { tr("Стоп", "Stop") } else { tr("Запись", "Record") }),
        (Tool::Picker, '\u{EF3C}', tr("Пипетка", "Picker")),
        (Tool::Plain, '\u{E77F}', tr("Вставка", "Paste")),
        (Tool::Ai, '\u{E99A}', tr("ИИ", "AI")),
    ];
    let top = y + 24.0;
    let (cw, ch) = (w / 4.0, (h - 24.0) / 2.0);
    for (k, (tool, glyph, label)) in tools.iter().enumerate() {
        let cx = x + (k % 4) as f32 * cw + cw / 2.0;
        let ty = top + (k / 4) as f32 * ch;
        let cy = ty + ch / 2.0 - 10.0;
        let hit = Hit::Tool(*tool);
        let active = *tool == Tool::Record && recording;
        let fill = if active { Some(RED.with_alpha(0.3)) } else { None };
        round_button(p, i, pal, *glyph, cx, cy, 25.0, hit, fill, if active { RED } else { pal.fg });
        p.text_c(label, Weight::Semibold, 11.0, cx - cw / 2.0, cy + 28.0, cw, 18.0, if i.hover == hit { pal.fg } else { pal.dim });
        p.hit(cx - cw / 2.0, ty, cw, ch, hit);
    }
}

fn parse_hex(hex: &str) -> Option<Rgba> {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(Rgba::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

fn draw_item_icon(p: &mut P, item: &settings::LauncherItem, pal: &Pal, x: f32, y: f32, size: f32) {
    if item.is_group() && item.icon.is_empty() {
        p.rect(x, y, size, size, 10.0, pal.chip);
        let cell = (size - 10.0) / 2.0;
        for (k, child) in item.children.iter().take(4).enumerate() {
            let cx = x + 4.0 + (k % 2) as f32 * (cell + 2.0);
            let cy = y + 4.0 + (k / 2) as f32 * (cell + 2.0);
            match launcher::icon(child) {
                Some(img) => p.image(&img, cx, cy, cell, cell, 3.0, 1.0),
                None => p.rect(cx, cy, cell, cell, 3.0, pal.chip_hover),
            }
        }
        return;
    }
    match launcher::icon(item) {
        Some(img) => p.image(&img, x, y, size, size, 8.0, 1.0),
        None => {
            p.rect(x, y, size, size, 10.0, pal.chip);
            let letter: String = item.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            p.text_c(&letter, Weight::Bold, size * 0.45, x, y, size, size, pal.fg);
        }
    }
}

fn draw_launcher(p: &mut P, i: &Island, pal: &Pal, x: f32, y: f32, w: f32, h: f32) {
    let items = settings::read(|s| s.launcher.clone());
    let cy = y + h / 2.0;
    let step = 44.0;
    let max = ((w - 60.0 - 140.0) / step) as usize;
    let mut hovered_name = None;
    for (k, item) in items.iter().take(max).enumerate() {
        let ix = x + k as f32 * step;
        let hit = Hit::Launch(k);
        let open = i.open_group.as_deref() == Some(&item.id);
        if i.hover == hit || open {
            p.rect(ix, cy - 21.0, 42.0, 42.0, 21.0, pal.chip_hover);
            hovered_name = Some(item.name.clone());
        }
        draw_item_icon(p, item, pal, ix + 5.0, cy - 16.0, 32.0);
        p.hit(ix, cy - 21.0, 42.0, 42.0, hit);
    }
    // Плюс — ссылка: после ярлыков, обведённым кружком
    let shown = items.len().min(max);
    let ax = x + shown as f32 * step + 21.0;
    round_button(p, i, pal, '\u{E710}', ax, cy, 17.0, Hit::LaunchAdd, None, pal.dim);
    let tx = ax + 30.0;
    if items.is_empty() {
        p.text_v(
            tr("Перетащите сюда ярлыки программ, папки или файлы; плюс — ссылка на сайт", "Drop app shortcuts, folders or files here; plus adds a link"),
            Weight::Regular,
            12.0,
            tx,
            cy,
            x + w - tx,
            pal.faint,
        );
        return;
    }
    if let Some(name) = hovered_name {
        p.text_v(&name, Weight::Semibold, 12.0, tx, cy, x + w - tx, pal.dim);
    }
    // Открытая группа — над рядом ярлыков
    if let Some(group) = items.iter().find(|g| Some(&g.id) == i.open_group.as_ref()) {
        let n = group.children.len();
        let gw = (n as f32 * 46.0 + 20.0).max(measure(&group.name, Weight::Semibold, 11.5) + 24.0);
        let gx = (x + items.iter().position(|g| g.id == group.id).unwrap_or(0) as f32 * step - 10.0).clamp(x, x + w - gw);
        let gy = y - 74.0;
        block(p, gx, gy, gw, 70.0, 18.0, pal, None);
        p.text_v(&group.name, Weight::Semibold, 11.5, gx + 12.0, gy + 13.0, gw - 24.0, pal.dim);
        for (k, child) in group.children.iter().enumerate() {
            let ix = gx + 10.0 + k as f32 * 46.0;
            let hit = Hit::GroupItem(k);
            if i.hover == hit {
                p.rect(ix - 2.0, gy + 24.0, 44.0, 42.0, 12.0, pal.chip_hover);
            }
            draw_item_icon(p, child, pal, ix + 4.0, gy + 28.0, 32.0);
            p.hit(ix - 2.0, gy + 24.0, 44.0, 42.0, hit);
        }
    }
}

fn draw_timer_tab(p: &mut P, i: &Island, pal: &Pal) {
    let (x, y, w, h) = (16.0, 66.0, 312.0, 262.0);
    block(p, x, y, w, h, 22.0, pal, None);
    p.text_v(tr("Таймер", "Timer"), Weight::Semibold, 14.0, x + 16.0, y + 22.0, 200.0, pal.fg);
    match &i.countdown {
        None => {
            let presets = timer_presets();
            let (bw, bh, gap) = ((w - 32.0 - 16.0) / 3.0, 44.0, 8.0);
            for (k, (minutes, label)) in presets.iter().enumerate() {
                let bx = x + 16.0 + (k % 3) as f32 * (bw + gap);
                let by = y + 52.0 + (k / 3) as f32 * (bh + gap);
                let hit = Hit::Preset(*minutes);
                if i.hover == hit {
                    p.rect(bx, by, bw, bh, bh / 2.0, pal.chip_hover);
                } else {
                    block(p, bx, by, bw, bh, bh / 2.0, pal, None);
                }
                p.text_c(label, Weight::Semibold, 13.0, bx, by, bw, bh, pal.fg);
                p.hit(bx, by, bw, bh, hit);
            }
            p.text_v(tr("Когда время выйдет — карточка со звуком", "A card with a sound when time is up"), Weight::Regular, 11.5, x + 16.0, y + h - 24.0, w - 32.0, pal.dim);
        }
        Some(cd) => {
            let left = cd.left();
            let paused = cd.paused.is_some();
            let progress = 1.0 - left.as_secs_f32() / cd.total.as_secs_f32().max(1.0);
            let (cx, cy) = (x + w / 2.0, y + 118.0);
            p.ring(cx, cy, 58.0, 6.0, pal.chip_hover);
            p.arc(cx, cy, 58.0, 0.0, std::f32::consts::TAU * (1.0 - progress), 6.0, ORANGE);
            p.text_c(&format_left(left), Weight::Semibold, 26.0, cx - 60.0, cy - 20.0, 120.0, 40.0, if paused { pal.dim } else { pal.fg });
            let buttons = [
                (Hit::TimerPause, if paused { '\u{E768}' } else { '\u{E769}' }),
                (Hit::TimerAdd, '\u{E710}'),
                (Hit::TimerStop, '\u{E711}'),
            ];
            for (k, (hit, glyph)) in buttons.iter().enumerate() {
                let bx = cx + (k as f32 - 1.0) * 52.0;
                let by = y + h - 40.0;
                p.circle(bx, by, 19.0, if i.hover == *hit { pal.chip_hover } else { pal.chip });
                p.icon(*glyph, 13.0, bx, by, pal.fg);
                p.hit(bx - 20.0, by - 20.0, 40.0, 40.0, *hit);
            }
        }
    }

    let (x, w) = (340.0, 364.0);
    block(p, x, y, w, h, 22.0, pal, None);
    p.text_v(tr("Напоминания", "Reminders"), Weight::Semibold, 14.0, x + 16.0, y + 22.0, 200.0, pal.fg);
    let list = crate::reminders::upcoming(4);
    if list.is_empty() {
        p.text_c(tr("Ближайших напоминаний нет", "No upcoming reminders"), Weight::Regular, 12.5, x, y + 100.0, w, 24.0, pal.dim);
    }
    let today = chrono::Local::now().date_naive();
    for (k, (at, r)) in list.iter().enumerate() {
        let ry = y + 44.0 + k as f32 * 36.0;
        let when = if at.date() == today {
            at.format("%H:%M").to_string()
        } else if at.date() == today.succ_opt().unwrap_or(today) {
            format!("{} {}", tr("завтра", "tmrw"), at.format("%H:%M"))
        } else {
            at.format("%d.%m %H:%M").to_string()
        };
        let cw = measure(&when, Weight::Semibold, 11.5) + 16.0;
        p.rect(x + 14.0, ry + 4.0, cw, 24.0, 12.0, pal.chip);
        p.text_c(&when, Weight::Semibold, 11.5, x + 14.0, ry + 4.0, cw, 24.0, ORANGE);
        p.text_v(&r.text, Weight::Regular, 12.5, x + 22.0 + cw, ry + 16.0, w - 36.0 - cw, pal.fg);
    }
    let by = y + h - 44.0;
    let quick = tr("Через 10 мин…", "In 10 min…");
    let qw = measure(quick, Weight::Semibold, 12.5) + 28.0;
    p.rect(x + 14.0, by, qw, 30.0, 15.0, if i.hover == Hit::QuickReminder { pal.chip_hover } else { pal.chip });
    p.text_c(quick, Weight::Semibold, 12.5, x + 14.0, by, qw, 30.0, pal.fg);
    p.hit(x + 14.0, by, qw, 30.0, Hit::QuickReminder);
    let all = tr("Все напоминания", "All reminders");
    let aw = measure(all, Weight::Semibold, 12.5) + 28.0;
    p.rect(x + w - 14.0 - aw, by, aw, 30.0, 15.0, if i.hover == Hit::AllReminders { pal.chip_hover } else { pal.chip });
    p.text_c(all, Weight::Semibold, 12.5, x + w - 14.0 - aw, by, aw, 30.0, pal.fg);
    p.hit(x + w - 14.0 - aw, by, aw, 30.0, Hit::AllReminders);
}

fn draw_feed_tab(p: &mut P, i: &Island, pal: &Pal) {
    let (x, y, w, h) = (16.0, 66.0, 688.0, 262.0);
    block(p, x, y, w, h, 22.0, pal, None);
    let mut row = 0;
    let rows = 6;
    for a in &i.agents {
        let ry = y + 10.0 + row as f32 * 37.0;
        let alpha = if a.status == Status::Working { pulse(3.0) } else { 1.0 };
        p.circle(x + 22.0, ry + 18.0, 5.0, agent_color(a.status).with_alpha(alpha));
        let status = match a.status {
            Status::Working => tr("работает", "working"),
            Status::Waiting => tr("ждёт ответа", "waiting"),
            Status::Done => tr("готово", "done"),
        };
        let nw = p.text_v(&format!("{} · {status}", a.name), Weight::Semibold, 13.0, x + 36.0, ry + 18.0, 260.0, pal.fg);
        p.text_v(&a.text, Weight::Regular, 12.5, x + 48.0 + nw, ry + 18.0, w - 64.0 - nw, pal.dim);
        row += 1;
    }
    for (k, (time, card)) in i.feed.iter().enumerate() {
        if row >= rows {
            break;
        }
        let ry = y + 10.0 + row as f32 * 37.0;
        let hit = Hit::Feed(k);
        if i.hover == hit {
            p.rect(x + 6.0, ry, w - 12.0, 36.0, 12.0, pal.chip);
        }
        p.icon(card.glyph, 13.0, x + 22.0, ry + 18.0, card.tint);
        p.text_v(time, Weight::Semibold, 11.5, x + 36.0, ry + 18.0, 40.0, pal.faint);
        let tw = p.text_v(&card.title, Weight::Semibold, 12.5, x + 76.0, ry + 18.0, 220.0, pal.fg);
        let body = card.body.lines().next().unwrap_or("");
        p.text_v(body, Weight::Regular, 12.5, x + 86.0 + tw, ry + 18.0, w - 100.0 - tw, pal.dim);
        p.hit(x + 6.0, ry, w - 12.0, 36.0, hit);
        row += 1;
    }
    if row == 0 {
        p.text_c(
            tr("Здесь появятся уведомления, ответы ИИ-агентов и напоминания", "Notifications, AI agent replies and reminders show up here"),
            Weight::Regular,
            12.5,
            x,
            y + h / 2.0 - 12.0,
            w,
            24.0,
            pal.dim,
        );
    }
}

// ---------- Значки ----------

/// Батарейка: x — левый край, cy — центр.
fn draw_battery(p: &mut P, x: f32, cy: f32, b: Battery, pal: &Pal) {
    let (w, h) = (21.0, 11.0);
    let y = cy - h / 2.0;
    p.stroke(x, y, w, h, 3.0, 1.2, pal.dim);
    p.rect(x + w + 1.0, cy - 2.0, 1.8, 4.0, 1.0, pal.dim);
    let color = if b.charging || b.plugged {
        GREEN
    } else if b.percent <= 20 {
        RED
    } else {
        pal.fg
    };
    let inner = (w - 4.0) * (b.percent as f32 / 100.0).clamp(0.05, 1.0);
    p.rect(x + 2.0, y + 2.0, inner, h - 4.0, 1.5, color);
    if b.charging || b.plugged {
        p.icon('\u{E945}', 9.0, x + w / 2.0, cy, Rgba::BLACK);
    }
}

/// Микрофон; перечёркнутый, если выключен.
fn draw_mic(p: &mut P, cx: f32, cy: f32, size: f32, muted: bool, color: Rgba) {
    p.icon('\u{E720}', size, cx, cy, color);
    if muted {
        let r = size * 0.55;
        p.line(&[(cx - r, cy - r), (cx + r, cy + r)], size * 0.12, color);
    }
}
