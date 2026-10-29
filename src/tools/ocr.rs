//! Текст с экрана: распознаватели Windows для русского и английского, результаты склеиваются построчно —
//! для каждой строки берётся вариант того языка, буквы которого в ней есть.

use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;

use super::capture::Shot;

struct OcrLine {
    text: String,
    top: f32,
    bottom: f32,
}

fn bitmap(shot: &Shot) -> windows::core::Result<SoftwareBitmap> {
    // Мелкий текст распознаётся заметно лучше, если картинку увеличить
    let factor = if shot.w.max(shot.h) < 1300 { 2 } else { 1 };
    let (w, h, data) = if factor == 1 {
        (shot.w, shot.h, shot.bgra.clone())
    } else {
        let img = image::RgbaImage::from_raw(shot.w, shot.h, shot.bgra.clone()).unwrap_or_default();
        let big = image::imageops::resize(&img, shot.w * 2, shot.h * 2, image::imageops::FilterType::CatmullRom);
        (shot.w * 2, shot.h * 2, big.into_raw())
    };
    let writer = DataWriter::new()?;
    writer.WriteBytes(&data)?;
    let buffer = writer.DetachBuffer()?;
    SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, w as i32, h as i32)
}

fn recognize(engine: &OcrEngine, bmp: &SoftwareBitmap) -> Vec<OcrLine> {
    let Ok(result) = engine.RecognizeAsync(bmp).and_then(|op| op.get()) else { return Vec::new() };
    let Ok(lines) = result.Lines() else { return Vec::new() };
    let mut out = Vec::new();
    for line in lines {
        let text = line.Text().map(|t| t.to_string()).unwrap_or_default();
        let (mut top, mut bottom) = (f32::MAX, f32::MIN);
        if let Ok(words) = line.Words() {
            for w in words {
                if let Ok(r) = w.BoundingRect() {
                    top = top.min(r.Y);
                    bottom = bottom.max(r.Y + r.Height);
                }
            }
        }
        if top == f32::MAX {
            top = out.len() as f32 * 20.0;
            bottom = top + 20.0;
        }
        out.push(OcrLine { text, top, bottom });
    }
    out
}

fn engine(tag: &str) -> Option<OcrEngine> {
    let lang = Language::CreateLanguage(&HSTRING::from(tag)).ok()?;
    if !OcrEngine::IsLanguageSupported(&lang).unwrap_or(false) {
        return None;
    }
    OcrEngine::TryCreateFromLanguage(&lang).ok()
}

fn has_cyrillic(s: &str) -> bool {
    s.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c))
}

/// Распознаёт текст. Блокирует поток (ждёт WinRT) — вызывать не из потока окон.
pub fn recognize_text(shot: &Shot) -> Result<String, String> {
    let bmp = bitmap(shot).map_err(|e| e.to_string())?;
    let ru = engine("ru");
    let en = engine("en-US").or_else(|| engine("en"));
    let mut ru_lines = ru.as_ref().map(|e| recognize(e, &bmp)).unwrap_or_default();
    let en_lines = en.as_ref().map(|e| recognize(e, &bmp)).unwrap_or_default();
    if ru.is_none() && en.is_none() {
        let any = OcrEngine::TryCreateFromUserProfileLanguages().map_err(|e| e.to_string())?;
        ru_lines = recognize(&any, &bmp);
    }
    // Каждой русской строке ищем английскую на той же высоте
    let mut merged: Vec<OcrLine> = Vec::new();
    let mut used = vec![false; en_lines.len()];
    for r in ru_lines {
        let center = (r.top + r.bottom) / 2.0;
        let pair = en_lines.iter().enumerate().find(|(i, e)| !used[*i] && center > e.top && center < e.bottom).map(|(i, _)| i);
        match pair {
            Some(i) => {
                used[i] = true;
                let e = &en_lines[i];
                let text = if has_cyrillic(&r.text) { r.text.clone() } else { e.text.clone() };
                merged.push(OcrLine { text, top: r.top.min(e.top), bottom: r.bottom.max(e.bottom) });
            }
            None => merged.push(r),
        }
    }
    for (i, e) in en_lines.into_iter().enumerate() {
        if !used[i] {
            merged.push(e);
        }
    }
    merged.sort_by(|a, b| a.top.partial_cmp(&b.top).unwrap_or(std::cmp::Ordering::Equal));
    let text = merged.iter().map(|l| l.text.trim()).filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n");
    if text.is_empty() {
        Err(crate::i18n::tr("Текст не найден", "No text found").into())
    } else {
        Ok(text)
    }
}
