//! Два языка: русский на русскоязычной Windows, английский на остальных; в настройках выбирается вручную.
//! Строки пишутся парой прямо в коде: `tr("Громкость", "Volume")`.

use std::sync::atomic::{AtomicU8, Ordering};

use crate::settings::{self, Language};

/// 0 — ещё не узнавали, 1 — русский, 2 — английский.
static SYSTEM: AtomicU8 = AtomicU8::new(0);

fn system_is_russian() -> bool {
    match SYSTEM.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let lang = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
            // Основной язык — младшие 10 бит: 0x19 — русский
            let ru = lang & 0x3FF == 0x19;
            SYSTEM.store(if ru { 1 } else { 2 }, Ordering::Relaxed);
            ru
        }
    }
}

pub fn is_english() -> bool {
    match settings::read(|s| s.language) {
        Language::Russian => false,
        Language::English => true,
        Language::Auto => !system_is_russian(),
    }
}

/// Строка на языке программы.
pub fn tr(ru: &'static str, en: &'static str) -> &'static str {
    if is_english() {
        en
    } else {
        ru
    }
}

/// То же для строк, собранных на лету.
pub fn trs(ru: impl Into<String>, en: impl Into<String>) -> String {
    if is_english() {
        en.into()
    } else {
        ru.into()
    }
}
