//! Цвета своих окон: светлая или тёмная тема (как в системе или выбранная в настройках) и акцент Windows.

use crate::gfx::Rgba;
use crate::settings::{self, ThemeMode};

pub fn is_dark() -> bool {
    match settings::read(|s| s.theme) {
        ThemeMode::Dark => true,
        ThemeMode::Light => false,
        ThemeMode::System => !crate::win::system_light_theme(),
    }
}

pub fn accent() -> Rgba {
    match crate::win::accent_color() {
        Some([r, g, b]) => Rgba::rgb(r, g, b),
        None => Rgba::rgb(0x4C, 0xC2, 0xFF),
    }
}

/// Палитра всплывающих окон (микшер у трея, индикатор громкости).
#[derive(Clone, Copy)]
pub struct Palette {
    pub background: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub secondary: Rgba,
    pub track: Rgba,
    pub hover: Rgba,
    pub accent: Rgba,
    pub peak: Rgba,
}

pub fn palette() -> Palette {
    let accent = accent();
    if is_dark() {
        Palette {
            background: Rgba::rgba(0x2B, 0x2B, 0x2D, 0xF5),
            border: Rgba::rgba(255, 255, 255, 0x22),
            text: Rgba::rgb(0xF2, 0xF2, 0xF2),
            secondary: Rgba::rgba(0xF2, 0xF2, 0xF2, 0x99),
            track: Rgba::rgba(255, 255, 255, 0x30),
            hover: Rgba::rgba(255, 255, 255, 0x14),
            accent,
            peak: accent.with_alpha(0.55),
        }
    } else {
        Palette {
            background: Rgba::rgba(0xF7, 0xF7, 0xF8, 0xF8),
            border: Rgba::rgba(0, 0, 0, 0x1C),
            text: Rgba::rgb(0x1B, 0x1B, 0x1B),
            secondary: Rgba::rgba(0x1B, 0x1B, 0x1B, 0x99),
            track: Rgba::rgba(0, 0, 0, 0x2A),
            hover: Rgba::rgba(0, 0, 0, 0x0D),
            accent,
            peak: accent.with_alpha(0.5),
        }
    }
}
