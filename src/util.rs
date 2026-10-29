//! Общие мелочи: папки программы, журнал ошибок, строки для WinAPI.

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

pub const APP_NAME: &str = "OmniFlex";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// %APPDATA%\OmniFlex — настройки, журнал ошибок, свои тексты песен и кэш.
pub fn data_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        base.join(APP_NAME)
    })
    .clone()
}

/// Ошибки пишутся в error.log рядом с настройками: программа живёт в трее, и окна с ошибкой никто бы не увидел.
pub fn log_error(error: impl std::fmt::Display) {
    let dir = data_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("error.log")) {
        let _ = writeln!(file, "[{}] {}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), error);
    }
}

/// Строка для WinAPI: UTF-16 с нулём на конце.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// UTF-16 из буфера WinAPI до первого нуля.
pub fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

pub fn starts_with_ignore_case(s: &str, prefix: &str) -> bool {
    let s = s.to_lowercase();
    s.starts_with(&prefix.to_lowercase())
}

/// Отрезает суффикс без учёта регистра; суффикс должен быть в конце строки.
pub fn strip_suffix_ignore_case<'a>(s: &'a str, suffix: &str) -> Option<&'a str> {
    if s.len() < suffix.len() {
        return None;
    }
    let cut = s.len() - suffix.len();
    if !s.is_char_boundary(cut) {
        return None;
    }
    if s[cut..].to_lowercase() == suffix.to_lowercase() {
        Some(&s[..cut])
    } else {
        None
    }
}

/// Обрезка заданных символов по краям (аналог string.Trim(params char[])).
pub fn trim_chars<'a>(s: &'a str, chars: &[char]) -> &'a str {
    s.trim_matches(|c| chars.contains(&c))
}

/// Миллисекунды с запуска системы — для коротких интервалов без системных часов.
pub fn tick_ms() -> u64 {
    unsafe { windows::Win32::System::SystemInformation::GetTickCount64() }
}
