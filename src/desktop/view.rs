//! Песня и её текст во весь экран — то, что показывается на рабочем столе вместо обоев.
//! Макет нарисован для 1920×1080 и масштабируется под экран; слева место под значки рабочего стола.
//! Кадр перерисовывается кусками: бегущая строка — только левая колонка, прокрутка текста — только правая.

use std::time::{Duration, Instant, SystemTime};

use tiny_skia::Pixmap;

use super::pointer::Control;
use crate::gfx::{self, ease_in_out_sine, ease_out_cubic, text, Canvas, Rgba, Tween, Weight};
use crate::media::lyrics::{self, Lyrics, Status};
use crate::media::{MusicSource, NowPlaying};
use crate::settings;

const PAGE_W: f32 = 1920.0;
const PAGE_H: f32 = 1080.0;
const COL_LEFT: f32 = 440.0;
const COL_W: f32 = 440.0;
const LYRICS_X: f32 = 1000.0;
const LYRICS_W: f32 = 790.0;
const LYRICS_MARGIN: f32 = 80.0;
/// Где по высоте стоит текущая строка (доля высоты области текста).
const ANCHOR: f32 = 0.38;
const CURRENT_OPACITY: f32 = 1.0;
const NEAR_OPACITY: f32 = 0.42;
const FAR_OPACITY: f32 = 0.22;
const PLAIN_OPACITY: f32 = 0.7;
/// Строку подсвечиваем чуть раньше: пока она доедет до места, певец как раз начнёт.
const LEAD: Duration = Duration::from_millis(250);
const COVER_SIZE: f32 = 440.0;
const BIG_COVER_W: f32 = 790.0;
const BIG_COVER_H: f32 = 760.0;
const LINE_SIZE: f32 = 42.0;
const LINE_HEIGHT: f32 = 54.0;
const LINE_GAP: f32 = 26.0;

#[derive(Clone, Copy, PartialEq)]
enum Region {
    Left,
    Right,
}

/// Бегущая строка, как в Spotify: длинный текст стоит, уезжает влево до конца, стоит и возвращается.
struct Marquee {
    overflow: f32,
    shift: Tween,
    forward: bool,
    wait_until: Option<Instant>,
    running: bool,
}

impl Marquee {
    fn new() -> Self {
        Self { overflow: 0.0, shift: Tween::new(0.0), forward: true, wait_until: None, running: false }
    }

    fn restart(&mut self, text_width: f32) {
        self.shift.set(0.0);
        self.forward = true;
        self.overflow = text_width - COL_W;
        self.wait_until = if self.running && self.overflow > 1.0 { Some(Instant::now() + Duration::from_secs_f32(2.5)) } else { None };
    }

    fn set_running(&mut self, running: bool, text_width: f32) {
        if self.running != running {
            self.running = running;
            self.restart(text_width);
        }
    }

    fn moving(&self) -> bool {
        self.shift.active()
    }

    /// Шаг времени: начать движение после паузы, развернуться в конце.
    fn update(&mut self, speed: f32) {
        if !self.running || self.overflow <= 1.0 {
            return;
        }
        if let Some(t) = self.wait_until {
            if Instant::now() >= t {
                self.wait_until = None;
                let target = if self.forward { -self.overflow } else { 0.0 };
                let distance = (target - self.shift.value()).abs();
                self.shift.animate(target, (distance / speed).max(0.5), ease_in_out_sine);
            }
            return;
        }
        if !self.shift.active() {
            self.forward = !self.forward;
            let pause = if self.forward { 2.5 } else { 1.5 };
            self.wait_until = Some(Instant::now() + Duration::from_secs_f32(pause));
        }
    }

    fn next_event(&self) -> Option<Instant> {
        self.wait_until
    }
}

struct WrappedLine {
    lines: Vec<text::Line>,
    top: f32,
    height: f32,
    opacity: Tween,
}

pub struct View {
    width: u32,
    height: u32,
    scale: f32,
    ox: f32,
    oy: f32,
    frame: Pixmap,
    background: Pixmap,
    track: Option<NowPlaying>,
    cover: Option<Pixmap>,
    cover_aspect: f32,
    cover_layer: Option<(Pixmap, f32)>,
    big_cover_layer: Option<(Pixmap, f32)>,
    backdrop: Option<Pixmap>,
    backdrop_next: Option<Pixmap>,
    backdrop_fade: Tween,
    page_alpha: Tween,
    lyrics: Lyrics,
    lines: Vec<WrappedLine>,
    lines_total: f32,
    current: i32,
    jump: bool,
    lines_shift: Tween,
    title_line: Option<text::Line>,
    artist_line: Option<text::Line>,
    title_marquee: Marquee,
    artist_marquee: Marquee,
    controls_visible: bool,
    pointer: (Option<Control>, bool),
    scrub: Option<f64>,
    scrub_hold: Option<Instant>,
    buttons: [Tween; 3],
    marquee_speed: f32,
    marquee_fps: i32,
    full_dirty: bool,
    dirty: Vec<Region>,
    last_second: u64,
    running: bool,
}

pub fn format_time(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

impl View {
    pub fn new(width: u32, height: u32) -> Self {
        let s = settings::get();
        let mut v = Self {
            width: 0,
            height: 0,
            scale: 1.0,
            ox: 0.0,
            oy: 0.0,
            frame: gfx::pixmap(1, 1),
            background: gfx::pixmap(1, 1),
            track: None,
            cover: None,
            cover_aspect: 1.0,
            cover_layer: None,
            big_cover_layer: None,
            backdrop: None,
            backdrop_next: None,
            backdrop_fade: Tween::new(1.0),
            page_alpha: Tween::new(1.0),
            lyrics: Lyrics::loading(),
            lines: Vec::new(),
            lines_total: 0.0,
            current: i32::MIN,
            jump: true,
            lines_shift: Tween::new(0.0),
            title_line: None,
            artist_line: None,
            title_marquee: Marquee::new(),
            artist_marquee: Marquee::new(),
            controls_visible: s.desktop_controls,
            pointer: (None, false),
            scrub: None,
            scrub_hold: None,
            buttons: [Tween::new(1.0), Tween::new(1.0), Tween::new(1.0)],
            marquee_speed: s.marquee_speed as f32,
            marquee_fps: s.marquee_fps,
            full_dirty: true,
            dirty: Vec::new(),
            last_second: u64::MAX,
            running: false,
        };
        v.resize(width, height);
        v
    }

    pub fn frame(&self) -> &Pixmap {
        &self.frame
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == self.width && height == self.height {
            return;
        }
        self.width = width.max(1);
        self.height = height.max(1);
        self.scale = (self.width as f32 / PAGE_W).min(self.height as f32 / PAGE_H);
        self.ox = (self.width as f32 - PAGE_W * self.scale) / 2.0;
        self.oy = (self.height as f32 - PAGE_H * self.scale) / 2.0;
        self.frame = gfx::pixmap(self.width, self.height);
        self.background = gfx::pixmap(self.width, self.height);
        self.rebuild_cover_layers();
        self.rewrap();
        self.render_background();
        self.full_dirty = true;
    }

    pub fn set_marquee(&mut self, speed: i32, fps: i32) {
        self.marquee_speed = speed as f32;
        self.marquee_fps = fps;
    }

    pub fn marquee_fps(&self) -> i32 {
        self.marquee_fps
    }

    pub fn set_controls(&mut self, visible: bool) {
        if self.controls_visible != visible {
            self.controls_visible = visible;
            if !visible {
                self.pointer = (None, false);
            }
            self.dirty(Region::Left);
        }
    }

    fn dirty(&mut self, r: Region) {
        if !self.dirty.contains(&r) {
            self.dirty.push(r);
        }
    }

    /// Точка макета 1920×1080 в пиксели окна.
    fn px(&self, x: f32) -> f32 {
        self.ox + x * self.scale
    }
    fn py(&self, y: f32) -> f32 {
        self.oy + y * self.scale
    }
    fn s(&self, v: f32) -> f32 {
        v * self.scale
    }

    // ---------- Данные ----------

    pub fn show_track(&mut self, track: NowPlaying) {
        let old = self.track.take();
        let new_key = old.as_ref().map(|o| o.key()) != Some(track.key());
        let cover_changed = old.as_ref().map(|o| !o.same_cover(&track)).unwrap_or(true);
        let playing_changed = old.as_ref().map(|o| o.is_playing != track.is_playing).unwrap_or(true);
        self.scrub_hold = None;
        self.track = Some(track.clone());
        self.running = true;
        if new_key {
            self.scrub = None;
            let title = text::layout(&track.title, Weight::Bold, self.s(46.0));
            let artist = text::layout(track.artist.as_deref().unwrap_or(""), Weight::Semibold, self.s(28.0));
            self.title_line = Some(title);
            self.artist_line = Some(artist);
            self.lyrics = Lyrics::loading();
            self.lines.clear();
            self.page_alpha.set(0.0);
            self.page_alpha.animate(1.0, 0.6, Tween::linear());
            self.restart_marquees();
            self.full_dirty = true;
        }
        if cover_changed {
            self.set_cover(track.cover.as_deref().map(|c| c.as_slice()));
        }
        let (tw, aw) = self.text_widths();
        self.title_marquee.set_running(true, tw);
        self.artist_marquee.set_running(true, aw);
        if playing_changed || new_key {
            self.dirty(Region::Left);
        }
        self.dirty(Region::Left);
        self.update_lines(true);
    }

    fn text_widths(&self) -> (f32, f32) {
        let tw = self.title_line.as_ref().map(|l| l.width / self.scale).unwrap_or(0.0);
        let aw = self.artist_line.as_ref().map(|l| l.width / self.scale).unwrap_or(0.0);
        (tw, aw)
    }

    fn restart_marquees(&mut self) {
        let (tw, aw) = self.text_widths();
        self.title_marquee.restart(tw);
        self.artist_marquee.restart(aw);
    }

    /// Заглушка между треками: следующий ролик YouTube ещё проверяется.
    pub fn show_placeholder(&mut self) {
        self.show_track(NowPlaying {
            title: "Загрузка…".into(),
            artist: None,
            album: None,
            cover: None,
            is_playing: true,
            position: Duration::ZERO,
            position_time: SystemTime::now(),
            duration: Duration::ZERO,
            source: MusicSource::None,
            video_title: None,
            cover_is_frame: false,
        });
    }

    pub fn show_lyrics(&mut self, key: &str, lyrics: Lyrics) {
        let Some(track) = &self.track else { return };
        if track.key() != key {
            return;
        }
        self.lyrics = lyrics;
        self.rewrap();
        self.current = i32::MIN;
        self.jump = true;
        self.dirty(Region::Right);
        self.dirty(Region::Left); // крупная обложка могла убрать маленькую
        self.update_lines(true);
    }

    pub fn stop(&mut self) {
        self.running = false;
        self.title_marquee.set_running(false, 0.0);
        self.artist_marquee.set_running(false, 0.0);
    }

    fn rewrap(&mut self) {
        self.lines.clear();
        let mut top = 0.0;
        if matches!(self.lyrics.status, Status::Synced | Status::Plain) {
            let width = self.s(LYRICS_W);
            let base = if self.lyrics.status == Status::Plain { PLAIN_OPACITY } else { FAR_OPACITY };
            for line in self.lyrics.lines.iter() {
                // Пустая строка в синхронном тексте — проигрыш
                let content = if line.text.is_empty() { "♪" } else { line.text.as_str() };
                let wrapped = text::wrap(content, Weight::Semibold, self.s(LINE_SIZE), width);
                let height = wrapped.len() as f32 * LINE_HEIGHT;
                self.lines.push(WrappedLine { lines: wrapped, top, height, opacity: Tween::new(base) });
                top += height + LINE_GAP;
            }
        }
        self.lines_total = top;
    }

    fn big_cover(&self) -> bool {
        self.track.as_ref().map(|t| t.source == MusicSource::YouTube).unwrap_or(false)
            && self.cover.is_some()
            && matches!(self.lyrics.status, Status::NotFound | Status::Error)
    }

    fn badge_height(&self) -> f32 {
        if self.lyrics.status == Status::Plain {
            18.0 * 1.33 + 10.0
        } else {
            0.0
        }
    }

    /// Область строк в макете: верх и высота.
    fn lines_area(&self) -> (f32, f32) {
        let top = LYRICS_MARGIN + self.badge_height();
        (top, PAGE_H - LYRICS_MARGIN - top)
    }

    fn set_cover(&mut self, data: Option<&[u8]>) {
        self.cover = data.and_then(|d| gfx::decode_image(d, 1400));
        self.cover_aspect = self
            .cover
            .as_ref()
            .map(|c| (c.width() as f32 / c.height().max(1) as f32).clamp(1.0, 16.0 / 9.0))
            .unwrap_or(1.0);
        self.rebuild_cover_layers();
        // Для фона хватает крошечной копии: растянутая, она уже почти размыта. Новый фон проявляется поверх старого
        let next = data.and_then(|d| gfx::decode_image_width(d, 24)).map(|mut p| {
            gfx::box_blur(&mut p, 2);
            p
        });
        if self.backdrop.is_none() && self.backdrop_next.is_none() {
            self.backdrop = next;
            self.backdrop_fade.set(1.0);
        } else {
            if self.backdrop_fade.active() {
                // Прошлая смена не закончилась — она становится текущим фоном
                if let Some(n) = self.backdrop_next.take() {
                    self.backdrop = Some(n);
                }
            }
            self.backdrop_next = next;
            self.backdrop_fade.set(0.0);
            self.backdrop_fade.animate(1.0, 0.9, Tween::linear());
        }
        self.render_background();
        self.full_dirty = true;
    }

    fn cover_box_height(&self) -> f32 {
        if self.cover.is_none() || self.cover_aspect < 1.15 {
            COVER_SIZE
        } else {
            COVER_SIZE / self.cover_aspect
        }
    }

    /// Обложка с тенью заранее — каждый кадр её только копируем.
    fn rebuild_cover_layers(&mut self) {
        let s = self.scale;
        let w = COVER_SIZE * s;
        let h = self.cover_box_height() * s;
        self.cover_layer = Some(self.make_cover_layer(w, h, 18.0 * s, 70.0 * s, 0.55, 18.0 * s));
        self.big_cover_layer = if self.cover.is_some() {
            let mut bw = BIG_COVER_W;
            let mut bh = bw / self.cover_aspect;
            if bh > BIG_COVER_H {
                bh = BIG_COVER_H;
                bw = BIG_COVER_H * self.cover_aspect;
            }
            Some(self.make_cover_layer(bw * s, bh * s, 22.0 * s, 80.0 * s, 0.6, 20.0 * s))
        } else {
            None
        };
    }

    fn make_cover_layer(&self, w: f32, h: f32, radius: f32, blur: f32, opacity: f32, depth: f32) -> (Pixmap, f32) {
        let (shadow, pad) = gfx::make_shadow(w, h, radius, blur / 2.0, opacity);
        let mut pm = gfx::pixmap(shadow.width(), shadow.height() + depth.ceil() as u32);
        {
            let mut c = Canvas::new(pm.as_mut());
            c.draw_pixmap(&shadow, 0.0, depth, 1.0);
            c.fill_round_rect(pad, pad, w, h, radius, Rgba::rgba(255, 255, 255, 0x26));
            match &self.cover {
                Some(img) => c.draw_image_fill(img, pad, pad, w, h, radius, 1.0),
                None => c.icon('\u{E8D6}', 120.0 * self.scale, pad + w / 2.0, pad + h / 2.0, Rgba::WHITE.with_alpha(0.5)),
            }
        }
        (pm, pad)
    }

    fn render_background(&mut self) {
        let (w, h) = (self.width as f32, self.height as f32);
        let mut c = Canvas::new(self.background.as_mut());
        c.fill(Rgba::rgb(0x10, 0x10, 0x14));
        let fade = self.backdrop_fade.value();
        let draw = |c: &mut Canvas, img: &Pixmap, opacity: f32| {
            // Растянуто с запасом 1.3, как «UniformToFill» с увеличением
            let scale = (w / img.width() as f32).max(h / img.height() as f32) * 1.3;
            let iw = img.width() as f32 * scale;
            let ih = img.height() as f32 * scale;
            c.draw_image_scaled(img, (w - iw) / 2.0, (h - ih) / 2.0, iw, ih, opacity);
        };
        if let Some(b) = &self.backdrop {
            draw(&mut c, b, 1.0);
        }
        if let Some(n) = &self.backdrop_next {
            draw(&mut c, n, fade);
        }
        c.fill_rect(0.0, 0.0, w, h, Rgba::rgba(0, 0, 0, 0x94));
        // Затемнение по краям: слева гуще (там значки), справа чуть меньше
        let stops = vec![
            tiny_skia::GradientStop::new(0.0, tiny_skia::Color::from_rgba8(0, 0, 0, 0x59)),
            tiny_skia::GradientStop::new(0.45, tiny_skia::Color::from_rgba8(0, 0, 0, 0)),
            tiny_skia::GradientStop::new(1.0, tiny_skia::Color::from_rgba8(0, 0, 0, 0x4D)),
        ];
        if let Some(shader) = tiny_skia::LinearGradient::new(
            tiny_skia::Point::from_xy(0.0, 0.0),
            tiny_skia::Point::from_xy(w, 0.0),
            stops,
            tiny_skia::SpreadMode::Pad,
            tiny_skia::Transform::identity(),
        ) {
            let paint = tiny_skia::Paint { shader, anti_alias: false, ..Default::default() };
            if let Some(rect) = tiny_skia::Rect::from_xywh(0.0, 0.0, w, h) {
                c.pm.fill_rect(rect, &paint, tiny_skia::Transform::identity(), None);
            }
        }
        if !self.backdrop_fade.active() {
            if let Some(n) = self.backdrop_next.take() {
                self.backdrop = Some(n);
            }
        }
    }

    // ---------- Мышь ----------

    /// Кнопки в пикселях окна (их ловит хук мыши по экранным координатам).
    pub fn control_bounds(&self) -> Vec<(Control, [f32; 4])> {
        if !self.controls_visible || self.track.is_none() {
            return Vec::new();
        }
        let l = self.left_layout();
        let mut out = vec![(Control::Seek, [self.px(COL_LEFT), self.py(l.progress), self.px(COL_LEFT + COL_W), self.py(l.progress + 30.0)])];
        for (i, control) in [Control::Previous, Control::PlayPause, Control::Next].iter().enumerate() {
            let bx = COL_LEFT + BUTTONS_X[i];
            out.push((*control, [self.px(bx), self.py(l.controls), self.px(bx + 72.0), self.py(l.controls + 72.0)]));
        }
        out
    }

    /// Подсветка кнопки под мышью; над полосой прогресса виден кружок, пока её тянут — время под мышью.
    pub fn set_pointer(&mut self, control: Option<Control>, pressed: bool, fraction: f64) {
        let scrub = if control == Some(Control::Seek) && pressed { Some(fraction.clamp(0.0, 1.0)) } else { None };
        if scrub != self.scrub && scrub.is_none() {
            // Отпустили. Новую позицию пришлёт плеер; если перемотка не вышла — через секунду вернёмся к настоящей
            self.scrub_hold = Some(Instant::now() + Duration::from_secs(1));
        }
        self.scrub = scrub;
        self.pointer = (control, pressed);
        for (i, c) in [Control::Previous, Control::PlayPause, Control::Next].iter().enumerate() {
            let hover = control == Some(*c);
            let target = if hover { if pressed { 0.94 } else { 1.06 } } else { 1.0 };
            self.buttons[i].animate(target, 0.12, ease_out_cubic);
        }
        self.dirty(Region::Left);
    }

    // ---------- Время и анимация ----------

    /// Шаг: что поменялось со временем. Возвращает, когда проснуться в следующий раз.
    pub fn update(&mut self) -> Option<Instant> {
        let track = self.track.clone()?;
        if !self.running {
            return None;
        }
        let now = SystemTime::now();
        let pos = track.position_at(now);
        let second = pos.as_secs();
        if second != self.last_second {
            self.last_second = second;
            self.dirty(Region::Left);
        }
        let mut next = Instant::now() + Duration::from_millis(1000 - pos.subsec_millis() as u64 + 15);

        self.update_lines(false);
        if self.lyrics.status == Status::Synced {
            let lines = &self.lyrics.lines;
            let current = self.current.max(-1);
            if let Some(l) = lines.get((current + 1) as usize) {
                let wait = l.time.saturating_sub(LEAD).saturating_sub(pos) + Duration::from_millis(30);
                next = next.min(Instant::now() + wait);
            }
        }

        let speed = self.marquee_speed;
        self.title_marquee.update(speed);
        self.artist_marquee.update(speed);
        if self.title_marquee.moving() || self.artist_marquee.moving() {
            self.dirty(Region::Left);
        }
        for m in [&self.title_marquee, &self.artist_marquee] {
            if let Some(t) = m.next_event() {
                next = next.min(t);
            }
        }
        if self.page_alpha.active() || self.backdrop_fade.active() {
            self.full_dirty = true;
        }
        if self.lines_shift.active() || self.lines.iter().any(|l| l.opacity.active()) {
            self.dirty(Region::Right);
        }
        if self.buttons.iter().any(|b| b.active()) {
            self.dirty(Region::Left);
        }
        if !track.is_playing && self.dirty.is_empty() && !self.full_dirty {
            // На паузе часы не идут — просыпаемся только ради анимаций
            next = self.title_marquee.next_event().or(self.artist_marquee.next_event()).unwrap_or(Instant::now() + Duration::from_secs(5));
        }
        Some(next)
    }

    /// Идёт ли анимация, которой нужны частые кадры; второе — только бегущая строка (её частота из настроек).
    pub fn animating(&self) -> (bool, bool) {
        let other = self.page_alpha.active()
            || self.backdrop_fade.active()
            || self.lines_shift.active()
            || self.lines.iter().any(|l| l.opacity.active())
            || self.buttons.iter().any(|b| b.active());
        let marquee = self.title_marquee.moving() || self.artist_marquee.moving();
        (other, marquee)
    }

    fn update_lines(&mut self, force: bool) {
        let Some(track) = &self.track else { return };
        let pos = track.position_at(SystemTime::now());
        let duration = track.duration;
        let (_, area_h) = self.lines_area();
        match self.lyrics.status {
            Status::Synced => {
                let lines = self.lyrics.lines.clone();
                let mut current: i32 = -1;
                while ((current + 1) as usize) < lines.len() && lines[(current + 1) as usize].time <= pos + LEAD {
                    current += 1;
                }
                if current != self.current || force && self.current == i32::MIN {
                    self.highlight(current, area_h);
                }
            }
            Status::Plain => {
                // Текст без меток времени листаем равномерно по ходу песни
                let range = (self.lines_total - area_h * 0.5).max(0.0);
                let share = if duration.is_zero() { 0.0 } else { (pos.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0) };
                let target = area_h * 0.15 - share * range;
                if self.jump {
                    self.lines_shift.set(target);
                    self.jump = false;
                    self.dirty(Region::Right);
                } else if (target - self.lines_shift.to).abs() > 0.5 {
                    self.lines_shift.animate(target, 1.0, Tween::linear());
                }
            }
            _ => {}
        }
    }

    fn highlight(&mut self, current: i32, area_h: f32) {
        self.current = current;
        for (i, l) in self.lines.iter_mut().enumerate() {
            let i = i as i32;
            let target = if current < 0 {
                if i == 0 {
                    NEAR_OPACITY
                } else {
                    FAR_OPACITY
                }
            } else if i == current {
                CURRENT_OPACITY
            } else if (i - current).abs() == 1 {
                NEAR_OPACITY
            } else {
                FAR_OPACITY
            };
            if self.jump {
                l.opacity.set(target);
            } else {
                l.opacity.animate(target, 0.35, Tween::linear());
            }
        }
        if let Some(line) = self.lines.get(current.max(0) as usize) {
            let target = area_h * ANCHOR - line.top - line.height / 2.0;
            if self.jump {
                self.lines_shift.set(target);
            } else {
                self.lines_shift.animate(target, 0.55, ease_out_cubic);
            }
        }
        self.jump = false;
        self.dirty(Region::Right);
    }

    // ---------- Рисование ----------

    /// Перерисовывает изменившееся. Возвращает прямоугольники окна, которые надо вывести на экран.
    pub fn render(&mut self) -> Vec<[i32; 4]> {
        if self.track.is_none() {
            return Vec::new();
        }
        if self.backdrop_fade.active() || self.backdrop_next.is_some() {
            self.render_background();
        }
        let mut out = Vec::new();
        let regions: Vec<[i32; 4]> = if self.full_dirty {
            vec![[0, 0, self.width as i32, self.height as i32]]
        } else {
            self.dirty
                .iter()
                .map(|r| match r {
                    // Тень обложки выходит за колонку — берём с запасом
                    Region::Left => self.rect_px(COL_LEFT - 90.0, 0.0, COL_W + 180.0, PAGE_H),
                    Region::Right => self.rect_px(LYRICS_X - 10.0, 0.0, LYRICS_W + 20.0 + 130.0, PAGE_H),
                })
                .collect()
        };
        self.full_dirty = false;
        self.dirty.clear();
        for r in regions {
            self.render_region(r);
            out.push(r);
        }
        out
    }

    fn rect_px(&self, x: f32, y: f32, w: f32, h: f32) -> [i32; 4] {
        let x0 = self.px(x).floor().max(0.0) as i32;
        let y0 = self.py(y).floor().max(0.0) as i32;
        let x1 = self.px(x + w).ceil().min(self.width as f32) as i32;
        let y1 = self.py(y + h).ceil().min(self.height as f32) as i32;
        [x0, y0, x1, y1]
    }

    fn render_region(&mut self, r: [i32; 4]) {
        let (w, h) = ((r[2] - r[0]).max(1) as u32, (r[3] - r[1]).max(1) as u32);
        let mut tmp = gfx::pixmap(w, h);
        // Фон
        copy_rect(&self.background, r[0], r[1], &mut tmp, 0, 0, w, h);
        {
            let mut c = Canvas::with_offset(tmp.as_mut(), r[0] as f32, r[1] as f32);
            c.alpha = self.page_alpha.value();
            self.draw_left(&mut c);
        }
        // Текст песни рисуется в своём слое и гаснет к краям
        self.draw_right(&mut tmp, r);
        copy_rect(&tmp, 0, 0, &mut self.frame, r[0], r[1], w, h);
    }

    fn left_layout(&self) -> LeftLayout {
        let big = self.big_cover();
        let cover_h = if big { 0.0 } else { self.cover_box_height() };
        let controls_h = if self.controls_visible { 22.0 + 72.0 } else { 0.0 };
        let total = cover_h + 40.0 + 58.0 + 8.0 + 38.0 + 4.0 + 29.0 + 22.0 + 30.0 + 27.0 + controls_h;
        let top = (PAGE_H - 40.0 - total) / 2.0;
        let title = top + cover_h + 40.0;
        let artist = title + 58.0 + 8.0;
        let album = artist + 38.0 + 4.0;
        let progress = album + 29.0 + 22.0;
        let times = progress + 30.0;
        let controls = times + 27.0 + 22.0;
        LeftLayout { top, title, artist, album, progress, times, controls }
    }

    fn draw_left(&self, c: &mut Canvas) {
        let Some(track) = &self.track else { return };
        let l = self.left_layout();
        let x = COL_LEFT;
        if !self.big_cover() {
            if let Some((layer, pad)) = &self.cover_layer {
                c.draw_pixmap(layer, self.px(x) - pad, self.py(l.top) - pad, 1.0);
            }
        }
        // Название и исполнитель: длинные уезжают влево; по вертикали не режем хвосты «у», «р»
        let clip_x0 = self.px(x);
        let clip_x1 = self.px(x + COL_W);
        if let Some(title) = &self.title_line {
            c.clip = Some([clip_x0, self.py(l.title - 58.0), clip_x1, self.py(l.title + 58.0 * 2.0)]);
            let shift = self.title_marquee.shift.value();
            c.draw_line(title, self.px(x + shift), self.py(l.title), Rgba::WHITE);
        }
        if let Some(artist) = &self.artist_line {
            c.clip = Some([clip_x0, self.py(l.artist - 38.0), clip_x1, self.py(l.artist + 38.0 * 2.0)]);
            let shift = self.artist_marquee.shift.value();
            c.draw_line(artist, self.px(x + shift), self.py(l.artist), Rgba::WHITE.with_alpha(0.78));
        }
        c.clip = None;
        if let Some(album) = track.album.as_ref().filter(|a| **a != track.title) {
            let line = text::ellipsize(album, Weight::Regular, self.s(22.0), self.s(COL_W));
            c.draw_line(&line, self.px(x), self.py(l.album), Rgba::WHITE.with_alpha(0.5));
        }

        // Полоса прогресса
        let pos = track.position_at(SystemTime::now());
        let held = self.scrub_hold.map(|t| Instant::now() < t).unwrap_or(false);
        let fraction = match self.scrub {
            Some(f) => f as f32,
            None if track.duration.is_zero() => 0.0,
            None => (pos.as_secs_f32() / track.duration.as_secs_f32()).clamp(0.0, 1.0),
        };
        let _ = held;
        let bar_y = l.progress + 12.0;
        c.fill_round_rect(self.px(x), self.py(bar_y), self.s(COL_W), self.s(6.0), self.s(3.0), Rgba::rgba(255, 255, 255, 0x3D));
        c.fill_round_rect(self.px(x), self.py(bar_y), self.s(COL_W * fraction), self.s(6.0), self.s(3.0), Rgba::rgba(255, 255, 255, 0xF2));
        if self.pointer.0 == Some(Control::Seek) {
            c.fill_circle(self.px(x + COL_W * fraction), self.py(bar_y + 3.0), self.s(9.0), Rgba::WHITE);
        }
        if !track.duration.is_zero() {
            let elapsed = match self.scrub {
                Some(f) => track.duration.mul_f64(f),
                None => pos,
            };
            let dim = Rgba::WHITE.with_alpha(0.6);
            c.text(&format_time(elapsed), Weight::Regular, self.s(20.0), self.px(x), self.py(l.times), dim);
            let total = format_time(track.duration);
            let tw = text::measure(&total, Weight::Regular, self.s(20.0));
            c.text(&total, Weight::Regular, self.s(20.0), self.px(x + COL_W) - tw, self.py(l.times), dim);
        }

        // Кнопки как в Spotify
        if self.controls_visible {
            let cy = l.controls + 36.0;
            let controls = [Control::Previous, Control::PlayPause, Control::Next];
            for (i, control) in controls.iter().enumerate() {
                let cx = x + BUTTONS_X[i] + 36.0;
                let scale = self.buttons[i].value();
                let hover = self.pointer.0 == Some(*control);
                match control {
                    Control::PlayPause => {
                        c.fill_circle(self.px(cx), self.py(cy), self.s(36.0 * scale), Rgba::WHITE);
                        let (glyph, shift) = if track.is_playing { ('\u{F8AE}', 0.0) } else { ('\u{F5B0}', 3.0) };
                        c.icon(glyph, self.s(28.0 * scale), self.px(cx + shift), self.py(cy), Rgba::BLACK);
                    }
                    _ => {
                        let glyph = if *control == Control::Previous { '\u{F8AC}' } else { '\u{F8AD}' };
                        let alpha = if hover { 1.0 } else { 0.75 };
                        c.icon(glyph, self.s(30.0 * scale), self.px(cx), self.py(cy), Rgba::WHITE.with_alpha(alpha));
                    }
                }
            }
        }
    }

    fn draw_right(&self, tmp: &mut Pixmap, r: [i32; 4]) {
        let alpha = self.page_alpha.value();
        let (area_top, area_h) = self.lines_area();
        {
            let mut c = Canvas::with_offset(tmp.as_mut(), r[0] as f32, r[1] as f32);
            c.alpha = alpha;
            if self.lyrics.status == Status::Plain {
                c.text("ТЕКСТ БЕЗ СИНХРОНИЗАЦИИ", Weight::Semibold, self.s(18.0), self.px(LYRICS_X), self.py(LYRICS_MARGIN), Rgba::WHITE.with_alpha(0.45));
            }
            if self.big_cover() {
                if let Some((layer, pad)) = &self.big_cover_layer {
                    let lh = (layer.height() as f32 - pad * 2.0) / self.scale;
                    let y = area_top + (area_h - lh) / 2.0;
                    c.draw_pixmap(layer, self.px(LYRICS_X) - pad, self.py(y) - pad, 1.0);
                }
                return;
            }
            if let Some((status, hint)) = self.status_text() {
                let width = self.s(LYRICS_W);
                let big = text::wrap(&status, Weight::Semibold, self.s(44.0), width);
                let small = if hint.is_empty() { Vec::new() } else { text::wrap(&hint, Weight::Regular, self.s(20.0), width) };
                let big_h = big.len() as f32 * 44.0 * 1.33;
                let small_h = if small.is_empty() { 0.0 } else { 16.0 + small.len() as f32 * 30.0 };
                let mut y = area_top + (area_h - big_h - small_h) / 2.0;
                for line in &big {
                    c.draw_line(line, self.px(LYRICS_X), self.py(y), Rgba::WHITE.with_alpha(0.55));
                    y += 44.0 * 1.33;
                }
                y += 16.0;
                for line in &small {
                    c.draw_line(line, self.px(LYRICS_X), self.py(y), Rgba::WHITE.with_alpha(0.4));
                    y += 30.0;
                }
                return;
            }
        }
        if self.lines.is_empty() {
            return;
        }
        // Строки — в отдельный слой размером с область текста, затем с затуханием по краям поверх фона
        let lx = self.px(LYRICS_X).floor() as i32;
        let ly = self.py(area_top).floor() as i32;
        let lw = self.s(LYRICS_W + 40.0).ceil() as u32;
        let lh = self.s(area_h).ceil() as u32;
        let mut layer = gfx::pixmap(lw, lh);
        {
            let mut c = Canvas::new(layer.as_mut());
            let shift = self.lines_shift.value();
            for wl in &self.lines {
                let top = wl.top + shift;
                if top + wl.height < -10.0 || top > area_h + 10.0 {
                    continue;
                }
                let color = Rgba::WHITE.with_alpha(wl.opacity.value());
                for (k, line) in wl.lines.iter().enumerate() {
                    let y = self.s(top + k as f32 * LINE_HEIGHT + (LINE_HEIGHT - LINE_SIZE * 1.33) / 2.0);
                    c.draw_line(line, 0.0, y, color);
                }
            }
        }
        // Маска: 0 → 16% проявляется, 80% → 100% гаснет
        let data = layer.data_mut();
        for row in 0..lh {
            let t = row as f32 / lh as f32;
            let m = if t < 0.16 {
                t / 0.16
            } else if t > 0.8 {
                (1.0 - t) / 0.2
            } else {
                1.0
            } * alpha;
            if (m - 1.0).abs() < 1e-3 {
                continue;
            }
            let start = (row * lw * 4) as usize;
            for v in &mut data[start..start + (lw * 4) as usize] {
                *v = (*v as f32 * m) as u8;
            }
        }
        if (alpha - 1.0).abs() < 1e-3 {
            // уже учтено в маске только для краёв — середина при полной видимости
        }
        let paint = tiny_skia::PixmapPaint::default();
        tmp.draw_pixmap(lx - r[0], ly - r[1], layer.as_ref(), &paint, tiny_skia::Transform::identity(), None);
    }

    fn status_text(&self) -> Option<(String, String)> {
        let track = self.track.as_ref()?;
        let (big, small) = match self.lyrics.status {
            Status::Loading => ("Ищу текст песни…".to_string(), String::new()),
            Status::Instrumental => ("Инструментальная композиция".to_string(), String::new()),
            Status::NotFound => {
                let name = match &track.artist {
                    Some(a) => format!("{a} - {}", track.title),
                    None => track.title.clone(),
                };
                let path = lyrics::folder().join(format!("{}.lrc", lyrics::safe_file_name(&name)));
                ("Текст не найден".to_string(), format!("Свой текст можно положить файлом\n{}", path.display()))
            }
            Status::Error => (
                "Не удалось загрузить текст".to_string(),
                "Нет связи с базами текстов — попробую ещё раз чуть позже".to_string(),
            ),
            _ => return None,
        };
        Some((big, small))
    }
}

/// Кнопки «назад», «играть», «вперёд»: отступ от левого края колонки.
const BUTTONS_X: [f32; 3] = [84.0, 184.0, 284.0];

struct LeftLayout {
    top: f32,
    title: f32,
    artist: f32,
    album: f32,
    progress: f32,
    times: f32,
    controls: f32,
}

/// Копирует прямоугольник пикселей между буферами (без смешивания).
fn copy_rect(src: &Pixmap, sx: i32, sy: i32, dst: &mut Pixmap, dx: i32, dy: i32, w: u32, h: u32) {
    let (sw, sh) = (src.width() as i32, src.height() as i32);
    let (dw, dh) = (dst.width() as i32, dst.height() as i32);
    let src_data = src.data();
    let dst_data = dst.data_mut();
    for row in 0..h as i32 {
        let (y_s, y_d) = (sy + row, dy + row);
        if y_s < 0 || y_s >= sh || y_d < 0 || y_d >= dh {
            continue;
        }
        let x_s = sx.max(0);
        let x_d = dx + (x_s - sx);
        let count = (w as i32 - (x_s - sx)).min(sw - x_s).min(dw - x_d);
        if count <= 0 || x_d < 0 {
            continue;
        }
        let s0 = ((y_s * sw + x_s) * 4) as usize;
        let d0 = ((y_d * dw + x_d) * 4) as usize;
        let n = (count * 4) as usize;
        dst_data[d0..d0 + n].copy_from_slice(&src_data[s0..s0 + n]);
    }
}
