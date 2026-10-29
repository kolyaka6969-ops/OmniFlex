//! QR-код с экрана.

use super::capture::Shot;

/// Содержимое всех QR-кодов в картинке.
pub fn decode(shot: &Shot) -> Vec<String> {
    let w = shot.w as usize;
    let luma = |x: usize, y: usize| -> u8 {
        let i = (y * w + x) * 4;
        let (b, g, r) = (shot.bgra[i] as u32, shot.bgra[i + 1] as u32, shot.bgra[i + 2] as u32);
        ((r * 299 + g * 587 + b * 114) / 1000) as u8
    };
    let mut found = Vec::new();
    // На светлом экране код тёмный; на тёмной теме бывает наоборот — пробуем и инверсию
    for invert in [false, true] {
        let mut img = rqrr::PreparedImage::prepare_from_greyscale(shot.w as usize, shot.h as usize, |x, y| {
            let v = luma(x, y);
            if invert {
                255 - v
            } else {
                v
            }
        });
        for grid in img.detect_grids() {
            if let Ok((_, content)) = grid.decode() {
                if !found.contains(&content) {
                    found.push(content);
                }
            }
        }
        if !found.is_empty() {
            break;
        }
    }
    found
}
