//! Рисование своих окон (остров, индикатор громкости, микшер у трея, обои с песней) на tiny-skia.
//! Пиксели хранятся в порядке BGRA с премультипликацией — так их без перекладки берут GDI и UpdateLayeredWindow,
//! поэтому все цвета и картинки сюда попадают уже с переставленными R и B.

pub mod text;

use tiny_skia::{
    FillRule, FilterQuality, Paint, PathBuilder, Pattern, Pixmap, PixmapMut, PixmapPaint, Rect, SpreadMode,
    Stroke, Transform,
};

pub use text::Weight;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    pub const WHITE: Rgba = Rgba::rgb(255, 255, 255);
    pub const BLACK: Rgba = Rgba::rgb(0, 0, 0);

    pub fn with_alpha(self, a: f32) -> Self {
        Self { a: (self.a as f32 * a.clamp(0.0, 1.0)).round() as u8, ..self }
    }

    /// Цвет для tiny-skia с переставленными R и B (буфер в BGRA).
    fn skia(self, alpha: f32) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.b, self.g, self.r, (self.a as f32 * alpha.clamp(0.0, 1.0)).round() as u8)
    }
}

/// Скруглённый прямоугольник как контур.
pub fn round_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut pb = PathBuilder::new();
    if r <= 0.01 {
        pb.push_rect(Rect::from_xywh(x, y, w, h)?);
        return pb.finish();
    }
    let k = 0.552_284_8 * r;
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Холст поверх пиксельного буфера. ox, oy — левый верхний угол буфера в координатах рисования:
/// так кусок кадра можно перерисовать в маленький буфер теми же вызовами, что и весь кадр.
pub struct Canvas<'a> {
    pub pm: PixmapMut<'a>,
    pub ox: f32,
    pub oy: f32,
    /// Общая прозрачность всего, что рисуется (появление страницы).
    pub alpha: f32,
    /// Обрезка текста: x0, y0, x1, y1 в координатах рисования (бегущая строка).
    pub clip: Option<[f32; 4]>,
}

impl<'a> Canvas<'a> {
    pub fn new(pm: PixmapMut<'a>) -> Self {
        Self { pm, ox: 0.0, oy: 0.0, alpha: 1.0, clip: None }
    }

    pub fn with_offset(pm: PixmapMut<'a>, ox: f32, oy: f32) -> Self {
        Self { pm, ox, oy, alpha: 1.0, clip: None }
    }

    fn transform(&self) -> Transform {
        Transform::from_translate(-self.ox, -self.oy)
    }

    pub fn fill(&mut self, color: Rgba) {
        self.pm.fill(color.skia(1.0));
    }

    fn paint(&self, color: Rgba) -> Paint<'static> {
        let mut paint = Paint::default();
        paint.set_color(color.skia(self.alpha));
        paint.anti_alias = true;
        paint
    }

    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgba) {
        if let Some(rect) = Rect::from_xywh(x, y, w, h) {
            let paint = self.paint(color);
            let t = self.transform();
            self.pm.fill_rect(rect, &paint, t, None);
        }
    }

    pub fn fill_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, color: Rgba) {
        if let Some(path) = round_rect_path(x, y, w, h, r) {
            let paint = self.paint(color);
            let t = self.transform();
            self.pm.fill_path(&path, &paint, FillRule::Winding, t, None);
        }
    }

    pub fn stroke_round_rect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, width: f32, color: Rgba) {
        if let Some(path) = round_rect_path(x, y, w, h, r) {
            let paint = self.paint(color);
            let t = self.transform();
            let stroke = Stroke { width, ..Stroke::default() };
            self.pm.stroke_path(&path, &paint, &stroke, t, None);
        }
    }

    pub fn fill_circle(&mut self, cx: f32, cy: f32, r: f32, color: Rgba) {
        if let Some(path) = PathBuilder::from_circle(cx, cy, r) {
            let paint = self.paint(color);
            let t = self.transform();
            self.pm.fill_path(&path, &paint, FillRule::Winding, t, None);
        }
    }

    pub fn stroke_circle(&mut self, cx: f32, cy: f32, r: f32, width: f32, color: Rgba) {
        if let Some(path) = PathBuilder::from_circle(cx, cy, r) {
            let paint = self.paint(color);
            let t = self.transform();
            let stroke = Stroke { width, ..Stroke::default() };
            self.pm.stroke_path(&path, &paint, &stroke, t, None);
        }
    }

    /// Ломаная линия со скруглёнными концами.
    pub fn stroke_polyline(&mut self, points: &[(f32, f32)], width: f32, color: Rgba) {
        if points.len() < 2 {
            if let Some(&(x, y)) = points.first() {
                self.fill_circle(x, y, width / 2.0, color);
            }
            return;
        }
        let mut pb = PathBuilder::new();
        pb.move_to(points[0].0, points[0].1);
        for p in &points[1..] {
            pb.line_to(p.0, p.1);
        }
        if let Some(path) = pb.finish() {
            let paint = self.paint(color);
            let t = self.transform();
            let stroke = Stroke {
                width,
                line_cap: tiny_skia::LineCap::Round,
                line_join: tiny_skia::LineJoin::Round,
                ..Stroke::default()
            };
            self.pm.stroke_path(&path, &paint, &stroke, t, None);
        }
    }

    pub fn fill_polygon(&mut self, points: &[(f32, f32)], color: Rgba) {
        if points.len() < 3 {
            return;
        }
        let mut pb = PathBuilder::new();
        pb.move_to(points[0].0, points[0].1);
        for p in &points[1..] {
            pb.line_to(p.0, p.1);
        }
        pb.close();
        if let Some(path) = pb.finish() {
            let paint = self.paint(color);
            let t = self.transform();
            self.pm.fill_path(&path, &paint, FillRule::Winding, t, None);
        }
    }

    pub fn stroke_rect(&mut self, x: f32, y: f32, w: f32, h: f32, width: f32, color: Rgba) {
        self.stroke_round_rect(x, y, w, h, 0.0, width, color);
    }

    /// Дуга окружности от угла start на sweep (радианы, 0 — вверх, по часовой).
    pub fn stroke_arc(&mut self, cx: f32, cy: f32, r: f32, start: f32, sweep: f32, width: f32, color: Rgba) {
        let steps = ((sweep.abs() / std::f32::consts::TAU) * 64.0).ceil().max(2.0) as usize;
        let mut pb = PathBuilder::new();
        for i in 0..=steps {
            let a = start + sweep * i as f32 / steps as f32;
            let (x, y) = (cx + r * a.sin(), cy - r * a.cos());
            if i == 0 {
                pb.move_to(x, y);
            } else {
                pb.line_to(x, y);
            }
        }
        if let Some(path) = pb.finish() {
            let paint = self.paint(color);
            let t = self.transform();
            let stroke = Stroke { width, line_cap: tiny_skia::LineCap::Round, ..Stroke::default() };
            self.pm.stroke_path(&path, &paint, &stroke, t, None);
        }
    }

    /// Картинка в прямоугольник: заполняет его целиком (лишнее по краям обрезается), углы скругляются.
    pub fn draw_image_fill(&mut self, image: &Pixmap, x: f32, y: f32, w: f32, h: f32, radius: f32, opacity: f32) {
        if image.width() == 0 || image.height() == 0 || w <= 0.0 || h <= 0.0 {
            return;
        }
        let scale = (w / image.width() as f32).max(h / image.height() as f32);
        let dx = x + (w - image.width() as f32 * scale) / 2.0;
        let dy = y + (h - image.height() as f32 * scale) / 2.0;
        let pattern_t = Transform::from_row(scale, 0.0, 0.0, scale, dx, dy);
        let Some(path) = round_rect_path(x, y, w, h, radius) else { return };
        let mut paint = Paint::default();
        paint.anti_alias = true;
        paint.shader =
            Pattern::new(image.as_ref(), SpreadMode::Pad, FilterQuality::Bilinear, opacity * self.alpha, pattern_t);
        let t = self.transform();
        self.pm.fill_path(&path, &paint, FillRule::Winding, t, None);
    }

    /// Картинка один к одному в точку (без масштаба).
    pub fn draw_pixmap(&mut self, image: &Pixmap, x: f32, y: f32, opacity: f32) {
        let paint = PixmapPaint { opacity: opacity * self.alpha, ..PixmapPaint::default() };
        let t = self.transform();
        let tx = (x + t.tx).round() as i32;
        let ty = (y + t.ty).round() as i32;
        self.pm.draw_pixmap(tx, ty, image.as_ref(), &paint, Transform::identity(), None);
    }

    /// Картинка с масштабом в прямоугольник, без обрезки и скруглений.
    pub fn draw_image_scaled(&mut self, image: &Pixmap, x: f32, y: f32, w: f32, h: f32, opacity: f32) {
        if image.width() == 0 || image.height() == 0 {
            return;
        }
        let sx = w / image.width() as f32;
        let sy = h / image.height() as f32;
        let t = self.transform().pre_translate(x, y).pre_scale(sx, sy);
        let paint =
            PixmapPaint { opacity: opacity * self.alpha, quality: FilterQuality::Bilinear, ..PixmapPaint::default() };
        self.pm.draw_pixmap(0, 0, image.as_ref(), &paint, t, None);
    }

    /// Строка текста; y — верх строки.
    pub fn draw_line(&mut self, line: &text::Line, x: f32, y: f32, color: Rgba) {
        let baseline = y + line.ascent;
        let alpha = color.a as f32 / 255.0 * self.alpha;
        if alpha <= 0.0 {
            return;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (0, 0, self.pm.width() as i32, self.pm.height() as i32);
        if let Some([cx0, cy0, cx1, cy1]) = self.clip {
            x0 = x0.max((cx0 - self.ox).floor() as i32);
            y0 = y0.max((cy0 - self.oy).floor() as i32);
            x1 = x1.min((cx1 - self.ox).ceil() as i32);
            y1 = y1.min((cy1 - self.oy).ceil() as i32);
        }
        let width = self.pm.width() as i32;
        let gx0 = x - self.ox;
        let gy = (baseline - self.oy).round() as i32;
        // Цвет уже в порядке буфера
        let (c0, c1, c2) = (color.b as f32, color.g as f32, color.r as f32);
        let data = self.pm.data_mut();
        for g in &line.glyphs {
            let gx = gx0 + g.x;
            let ix = gx.floor();
            let sub = ((gx - ix) * 4.0).floor().clamp(0.0, 3.0) as u8;
            let Some(bitmap) = text::glyph_bitmap(g.font, g.id, line.size, sub) else { continue };
            let left = ix as i32 + bitmap.left;
            let top = gy + bitmap.top;
            if left >= x1 || top >= y1 || left + bitmap.width as i32 <= x0 || top + bitmap.height as i32 <= y0 {
                continue;
            }
            for row in 0..bitmap.height as i32 {
                let py = top + row;
                if py < y0 || py >= y1 {
                    continue;
                }
                for col in 0..bitmap.width as i32 {
                    let px = left + col;
                    if px < x0 || px >= x1 {
                        continue;
                    }
                    let cov = bitmap.coverage[(row as u32 * bitmap.width + col as u32) as usize];
                    if cov == 0 {
                        continue;
                    }
                    let a = cov as f32 / 255.0 * alpha;
                    let i = ((py * width + px) * 4) as usize;
                    let inv = 1.0 - a;
                    data[i] = (c0 * a + data[i] as f32 * inv).round().min(255.0) as u8;
                    data[i + 1] = (c1 * a + data[i + 1] as f32 * inv).round().min(255.0) as u8;
                    data[i + 2] = (c2 * a + data[i + 2] as f32 * inv).round().min(255.0) as u8;
                    data[i + 3] = (255.0 * a + data[i + 3] as f32 * inv).round().min(255.0) as u8;
                }
            }
        }
    }

    /// Текст одной строкой; y — верх строки. Возвращает ширину.
    pub fn text(&mut self, s: &str, weight: Weight, size: f32, x: f32, y: f32, color: Rgba) -> f32 {
        let line = text::layout(s, weight, size);
        self.draw_line(&line, x, y, color);
        line.width
    }

    /// Текст по центру прямоугольника.
    pub fn text_centered(&mut self, s: &str, weight: Weight, size: f32, x: f32, y: f32, w: f32, h: f32, color: Rgba) {
        let line = text::layout(s, weight, size);
        let (ascent, height) = text::metrics(size, weight);
        let _ = ascent;
        self.draw_line(&line, x + (w - line.width) / 2.0, y + (h - height) / 2.0, color);
    }

    /// Значок по центру прямоугольника. У значков Segoe Fluent квадрат em, а не метрики текста.
    pub fn icon(&mut self, glyph: char, size: f32, cx: f32, cy: f32, color: Rgba) {
        let line = text::layout(&glyph.to_string(), Weight::Icon, size);
        let (ascent, _) = text::metrics(size, Weight::Icon);
        // У Segoe Fluent Icons глиф занимает em над базовой линией
        self.draw_line(&line, cx - line.width / 2.0, cy + size / 2.0 - ascent, color);
    }
}

/// Пустой буфер нужного размера (минимум 1×1).
pub fn pixmap(width: u32, height: u32) -> Pixmap {
    Pixmap::new(width.max(1), height.max(1)).expect("pixmap size")
}

/// Картинка из PNG/JPEG в буфер BGRA с премультипликацией; max_side — ужать, если больше (0 — как есть).
pub fn decode_image(data: &[u8], max_side: u32) -> Option<Pixmap> {
    let img = image::load_from_memory(data).ok()?;
    let img = if max_side > 0 && (img.width() > max_side || img.height() > max_side) {
        img.resize(max_side, max_side, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    Some(rgba_to_pixmap(&img.to_rgba8()))
}

/// Картинка, ужатая до ширины width с сохранением пропорций (для размытого фона).
pub fn decode_image_width(data: &[u8], width: u32) -> Option<Pixmap> {
    let img = image::load_from_memory(data).ok()?;
    let height = ((img.height() as f32 * width as f32 / img.width().max(1) as f32).round() as u32).max(1);
    let img = img.resize_exact(width, height, image::imageops::FilterType::Triangle);
    Some(rgba_to_pixmap(&img.to_rgba8()))
}

/// Размеры картинки без полного декодирования.
pub fn image_size(data: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(data)).with_guessed_format().ok()?.into_dimensions().ok()
}

pub fn rgba_to_pixmap(img: &image::RgbaImage) -> Pixmap {
    let mut pm = pixmap(img.width(), img.height());
    let data = pm.data_mut();
    for (i, p) in img.pixels().enumerate() {
        let a = p[3] as u32;
        let o = i * 4;
        data[o] = ((p[2] as u32 * a + 127) / 255) as u8;
        data[o + 1] = ((p[1] as u32 * a + 127) / 255) as u8;
        data[o + 2] = ((p[0] as u32 * a + 127) / 255) as u8;
        data[o + 3] = a as u8;
    }
    pm
}

/// Значок приложения (RGBA без премультипликации) в буфер.
pub fn icon_to_pixmap(width: u32, height: u32, rgba: &[u8]) -> Option<Pixmap> {
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    Some(rgba_to_pixmap(&img))
}

/// Размытие тремя проходами коробочного фильтра (почти гауссово), по месту.
pub fn box_blur(pm: &mut Pixmap, radius: u32) {
    if radius == 0 {
        return;
    }
    let w = pm.width() as usize;
    let h = pm.height() as usize;
    let mut tmp = vec![0u8; w * h * 4];
    for _ in 0..3 {
        blur_pass(pm.data(), &mut tmp, w, h, radius as usize, true);
        let data = pm.data_mut();
        blur_pass(&tmp, data, w, h, radius as usize, false);
    }
}

fn blur_pass(src: &[u8], dst: &mut [u8], w: usize, h: usize, r: usize, horizontal: bool) {
    let (outer, inner) = if horizontal { (h, w) } else { (w, h) };
    let idx = |o: usize, i: usize| if horizontal { (o * w + i) * 4 } else { (i * w + o) * 4 };
    let span = (2 * r + 1) as u32;
    for o in 0..outer {
        let mut sum = [0u32; 4];
        for k in 0..=2 * r {
            let i = k.saturating_sub(r).min(inner - 1);
            let p = idx(o, i);
            for c in 0..4 {
                sum[c] += src[p + c] as u32;
            }
        }
        for i in 0..inner {
            let p = idx(o, i);
            for c in 0..4 {
                dst[p + c] = (sum[c] / span) as u8;
            }
            let out = i.saturating_sub(r).min(inner - 1);
            let inn = (i + r + 1).min(inner - 1);
            let po = idx(o, out);
            let pi = idx(o, inn);
            for c in 0..4 {
                sum[c] = sum[c] + src[pi + c] as u32 - src[po + c] as u32;
            }
        }
    }
}

/// Тень под скруглённым прямоугольником: размытая тёмная подложка. Возвращает буфер и отступ (на сколько он
/// больше прямоугольника с каждой стороны).
pub fn make_shadow(w: f32, h: f32, radius: f32, blur: f32, opacity: f32) -> (Pixmap, f32) {
    let pad = blur.ceil() + 2.0;
    let mut pm = pixmap((w + pad * 2.0).ceil() as u32, (h + pad * 2.0).ceil() as u32);
    {
        let mut canvas = Canvas::new(pm.as_mut());
        canvas.fill_round_rect(pad, pad, w, h, radius, Rgba::BLACK.with_alpha(opacity));
    }
    box_blur(&mut pm, (blur / 3.0).max(1.0) as u32);
    (pm, pad)
}

/// Сглаживание анимаций.
pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

pub fn ease_in_out_sine(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    -((std::f32::consts::PI * t).cos() - 1.0) / 2.0
}

/// Анимируемое число: плавно идёт к цели за заданное время.
#[derive(Clone, Copy, Debug)]
pub struct Tween {
    pub from: f32,
    pub to: f32,
    pub start: std::time::Instant,
    pub duration: f32,
    pub ease: fn(f32) -> f32,
}

fn linear(t: f32) -> f32 {
    t.clamp(0.0, 1.0)
}

impl Tween {
    pub fn new(value: f32) -> Self {
        Self { from: value, to: value, start: std::time::Instant::now(), duration: 0.0, ease: linear }
    }

    pub fn value(&self) -> f32 {
        if self.duration <= 0.0 {
            return self.to;
        }
        let t = self.start.elapsed().as_secs_f32() / self.duration;
        if t >= 1.0 {
            self.to
        } else {
            self.from + (self.to - self.from) * (self.ease)(t)
        }
    }

    pub fn active(&self) -> bool {
        self.duration > 0.0 && self.start.elapsed().as_secs_f32() < self.duration
    }

    pub fn animate(&mut self, to: f32, duration: f32, ease: fn(f32) -> f32) {
        if (to - self.to).abs() < 1e-4 && (self.active() || (self.value() - to).abs() < 1e-4) {
            return;
        }
        self.from = self.value();
        self.to = to;
        self.start = std::time::Instant::now();
        self.duration = duration;
        self.ease = ease;
    }

    pub fn set(&mut self, value: f32) {
        self.from = value;
        self.to = value;
        self.duration = 0.0;
    }

    pub fn linear() -> fn(f32) -> f32 {
        linear
    }
}
