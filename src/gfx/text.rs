//! Текст для своих окон: шрифты Segoe UI из системы, подстановка недостающих символов
//! (символы, эмодзи, китайский, японский, корейский) и растровый кэш глифов.

use ab_glyph::{point, Font, FontArc, FontVec, GlyphId, PxScale, ScaleFont};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Weight {
    Regular,
    Semibold,
    Bold,
    /// Значки Segoe Fluent Icons (или Segoe MDL2 Assets в Windows 10).
    Icon,
}

struct FontSlot {
    file: &'static str,
    index: u32,
    font: OnceLock<Option<FontArc>>,
}

impl FontSlot {
    const fn new(file: &'static str, index: u32) -> Self {
        Self { file, index, font: OnceLock::new() }
    }

    fn get(&self) -> Option<&FontArc> {
        self.font
            .get_or_init(|| {
                let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
                let data = std::fs::read(format!("{dir}\\Fonts\\{}", self.file)).ok()?;
                FontVec::try_from_vec_and_index(data, self.index).ok().map(FontArc::new)
            })
            .as_ref()
    }
}

// Номера шрифтов в кэше глифов — индексы в этом списке
static FONTS: [FontSlot; 11] = [
    FontSlot::new("segoeui.ttf", 0),    // 0 Regular
    FontSlot::new("seguisb.ttf", 0),    // 1 Semibold
    FontSlot::new("segoeuib.ttf", 0),   // 2 Bold
    FontSlot::new("SegoeIcons.ttf", 0), // 3 Icon (Windows 11)
    FontSlot::new("segmdl2.ttf", 0),    // 4 Icon (Windows 10)
    FontSlot::new("seguisym.ttf", 0),   // 5 символы: ♪ ★ стрелки
    FontSlot::new("seguiemj.ttf", 0),   // 6 эмодзи (контуром)
    FontSlot::new("msyh.ttc", 0),       // 7 китайский
    FontSlot::new("YuGothM.ttc", 0),    // 8 японский
    FontSlot::new("malgun.ttf", 0),     // 9 корейский
    FontSlot::new("arial.ttf", 0),      // 10 на всякий случай
];

const FALLBACKS: [usize; 6] = [5, 6, 7, 8, 9, 10];

/// Путь к файлу шрифта (для egui).
pub fn font_bytes(file: &str) -> Option<Vec<u8>> {
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    std::fs::read(format!("{dir}\\Fonts\\{file}")).ok()
}

fn primary(weight: Weight) -> usize {
    match weight {
        Weight::Regular => 0,
        Weight::Semibold => {
            if FONTS[1].get().is_some() {
                1
            } else {
                0
            }
        }
        Weight::Bold => {
            if FONTS[2].get().is_some() {
                2
            } else {
                0
            }
        }
        Weight::Icon => {
            if FONTS[3].get().is_some() {
                3
            } else {
                4
            }
        }
    }
}

/// Масштаб ab_glyph для кегля em (как FontSize в WPF).
fn px_scale(font: &FontArc, em: f32) -> PxScale {
    let upem = font.units_per_em().unwrap_or(2048.0);
    PxScale::from(em * font.height_unscaled() / upem)
}

/// Шрифт, в котором есть символ: сначала основной, потом подстановки.
fn pick(weight: Weight, c: char) -> Option<(usize, GlyphId)> {
    let first = primary(weight);
    if let Some(font) = FONTS[first].get() {
        let id = font.glyph_id(c);
        if id.0 != 0 {
            return Some((first, id));
        }
    }
    if weight == Weight::Icon {
        return None;
    }
    for &i in &FALLBACKS {
        if let Some(font) = FONTS[i].get() {
            let id = font.glyph_id(c);
            if id.0 != 0 {
                return Some((i, id));
            }
        }
    }
    FONTS[first].get().map(|f| (first, f.glyph_id(c)))
}

#[derive(Clone, Debug)]
pub struct PlacedGlyph {
    pub font: usize,
    pub id: GlyphId,
    pub x: f32,
}

/// Одна строка текста, уже разложенная по глифам.
#[derive(Clone, Debug)]
pub struct Line {
    pub glyphs: Vec<PlacedGlyph>,
    pub width: f32,
    pub size: f32,
    /// Расстояние от верха строки до базовой линии.
    pub ascent: f32,
}

/// Высота строки и положение базовой линии для кегля (по метрикам Segoe UI).
pub fn metrics(size: f32, weight: Weight) -> (f32, f32) {
    match FONTS[primary(weight)].get() {
        Some(font) => {
            let scaled = font.as_scaled(px_scale(font, size));
            (scaled.ascent(), scaled.ascent() - scaled.descent())
        }
        None => (size * 1.08, size * 1.33),
    }
}

pub fn layout(text: &str, weight: Weight, size: f32) -> Line {
    let (ascent, _) = metrics(size, weight);
    let mut glyphs = Vec::with_capacity(text.len());
    let mut x = 0.0f32;
    let mut prev: Option<(usize, GlyphId)> = None;
    for c in text.chars() {
        if c == '\n' || c == '\r' {
            continue;
        }
        let Some((fi, id)) = pick(weight, c) else { continue };
        let Some(font) = FONTS[fi].get() else { continue };
        let scaled = font.as_scaled(px_scale(font, size));
        if let Some((pf, pid)) = prev {
            if pf == fi {
                x += scaled.kern(pid, id);
            }
        }
        glyphs.push(PlacedGlyph { font: fi, id, x });
        x += scaled.h_advance(id);
        prev = Some((fi, id));
    }
    Line { glyphs, width: x, size, ascent }
}

pub fn measure(text: &str, weight: Weight, size: f32) -> f32 {
    layout(text, weight, size).width
}

/// Перенос по словам в пределах ширины; слишком длинное слово режется по символам.
pub fn wrap(text: &str, weight: Weight, size: f32, max_width: f32) -> Vec<Line> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split(' ') {
            let candidate = if current.is_empty() { word.to_string() } else { format!("{current} {word}") };
            if measure(&candidate, weight, size) <= max_width || current.is_empty() && measure(word, weight, size) <= max_width {
                current = candidate;
                continue;
            }
            if !current.is_empty() {
                lines.push(layout(&current, weight, size));
                current.clear();
            }
            if measure(word, weight, size) <= max_width {
                current = word.to_string();
            } else {
                // Слово шире строки — режем по символам
                for c in word.chars() {
                    let candidate = format!("{current}{c}");
                    if !current.is_empty() && measure(&candidate, weight, size) > max_width {
                        lines.push(layout(&current, weight, size));
                        current = c.to_string();
                    } else {
                        current = candidate;
                    }
                }
            }
        }
        lines.push(layout(&current, weight, size));
    }
    lines
}

/// Обрезает строку с многоточием, чтобы она влезла в ширину.
pub fn ellipsize(text: &str, weight: Weight, size: f32, max_width: f32) -> Line {
    let line = layout(text, weight, size);
    if line.width <= max_width {
        return line;
    }
    let chars: Vec<char> = text.chars().collect();
    let mut lo = 0usize;
    let mut hi = chars.len();
    while lo < hi {
        let mid = (lo + hi + 1) / 2;
        let candidate: String = chars[..mid].iter().collect::<String>() + "…";
        if measure(&candidate, weight, size) <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let s: String = chars[..lo].iter().collect::<String>().trim_end().to_string() + "…";
    layout(&s, weight, size)
}

/// Растр глифа: покрытие 0..255, смещение левого верхнего угла от точки начала глифа на базовой линии.
pub struct GlyphBitmap {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub coverage: Vec<u8>,
}

thread_local! {
    static CACHE: RefCell<HashMap<(usize, u16, u32, u8), Option<Rc<GlyphBitmap>>>> = RefCell::new(HashMap::new());
}

/// Растр глифа из кэша потока; subpixel — четверть пикселя по горизонтали (0..3).
pub fn glyph_bitmap(font_index: usize, id: GlyphId, size: f32, subpixel: u8) -> Option<Rc<GlyphBitmap>> {
    let key = (font_index, id.0, (size * 4.0).round() as u32, subpixel);
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(found) = cache.get(&key) {
            return found.clone();
        }
        if cache.len() > 6000 {
            cache.clear();
        }
        let result = rasterize(font_index, id, size, subpixel).map(Rc::new);
        cache.insert(key, result.clone());
        result
    })
}

fn rasterize(font_index: usize, id: GlyphId, size: f32, subpixel: u8) -> Option<GlyphBitmap> {
    let font = FONTS[font_index].get()?;
    let glyph = id.with_scale_and_position(px_scale(font, size), point(subpixel as f32 * 0.25, 0.0));
    let outlined = font.outline_glyph(glyph)?;
    let bounds = outlined.px_bounds();
    let width = bounds.width().ceil().max(0.0) as u32;
    let height = bounds.height().ceil().max(0.0) as u32;
    if width == 0 || height == 0 {
        return None;
    }
    let mut coverage = vec![0u8; (width * height) as usize];
    outlined.draw(|x, y, c| {
        if x < width && y < height {
            coverage[(y * width + x) as usize] = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    });
    Some(GlyphBitmap { left: bounds.min.x.floor() as i32, top: bounds.min.y.floor() as i32, width, height, coverage })
}
