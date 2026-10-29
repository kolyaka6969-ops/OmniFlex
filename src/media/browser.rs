//! Узнаёт, что играет в браузере. Windows отдаёт в медиа-сеансах только программу, а не сайт,
//! поэтому смотрим заголовки окон браузеров: у вкладки Яндекс Музыки в нём есть «Яндекс Музыка»,
//! у YouTube Music — «YouTube Music», у обычного ролика — «YouTube».

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, IsWindowVisible};

use crate::win;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BrowserMusic {
    #[default]
    None,
    YouTube,
    YouTubeMusic,
    YandexMusic,
}

/// Сколько помним, что музыкальный сайт видели: вкладка может уйти в фон, а песня — продолжаться.
const MEMORY: Duration = Duration::from_secs(15 * 60);
const CHECK_INTERVAL: Duration = Duration::from_secs(2);

pub const BROWSER_EXES: &[&str] = &["chrome", "chromium", "msedge", "firefox", "opera", "brave", "vivaldi", "browser", "yandexbrowser"];

pub struct BrowserMusicDetector {
    windows: Vec<(String, BrowserMusic)>,
    checked_at: Option<Instant>,
    last_seen: Option<Instant>,
    kind: BrowserMusic,
}

pub fn looks_like_yandex_music(text: &str) -> bool {
    let lower = text.to_lowercase();
    (lower.contains("яндекс") || lower.contains("yandex")) && (lower.contains("музык") || lower.contains("music"))
}

pub fn classify_title(title: &str) -> BrowserMusic {
    if title.trim().is_empty() {
        return BrowserMusic::None;
    }
    let lower = title.to_lowercase();
    if looks_like_yandex_music(title) {
        BrowserMusic::YandexMusic
    } else if lower.contains("youtube music") {
        BrowserMusic::YouTubeMusic
    } else if lower.contains("youtube") {
        BrowserMusic::YouTube
    } else {
        BrowserMusic::None
    }
}

/// Сайты по важности: музыкальные плееры важнее обычного YouTube.
fn rank(kind: BrowserMusic) -> i32 {
    match kind {
        BrowserMusic::YandexMusic => 3,
        BrowserMusic::YouTubeMusic => 2,
        BrowserMusic::YouTube => 1,
        BrowserMusic::None => 0,
    }
}

impl BrowserMusicDetector {
    pub fn new() -> Self {
        Self { windows: Vec::new(), checked_at: None, last_seen: None, kind: BrowserMusic::None }
    }

    /// Что за музыкальный сайт видели только что или недавно.
    pub fn check(&mut self) -> BrowserMusic {
        self.refresh();
        match self.last_seen {
            Some(t) if t.elapsed() < MEMORY => self.kind,
            _ => BrowserMusic::None,
        }
    }

    /// Уточняет сайт по названию песни: если оно есть в заголовке окна, значит вкладка на виду.
    pub fn match_title(&mut self, title: Option<&str>) -> BrowserMusic {
        let Some(title) = title.filter(|t| !t.trim().is_empty()) else { return BrowserMusic::None };
        let needle = title.trim().to_lowercase();
        // Слишком короткое название встречается в любом заголовке
        if needle.chars().count() < 6 {
            return BrowserMusic::None;
        }
        self.refresh();
        if let Some((_, k)) = self.windows.iter().find(|(t, _)| t.to_lowercase().contains(&needle)) {
            return *k;
        }
        // Вкладка, возможно, только переключилась — опрашиваем окна повторно
        if self.checked_at.is_some() {
            self.checked_at = None;
            self.refresh();
            if let Some((_, k)) = self.windows.iter().find(|(t, _)| t.to_lowercase().contains(&needle)) {
                return *k;
            }
        }
        BrowserMusic::None
    }

    /// Продлевает память, пока браузерный сеанс считается музыкой.
    pub fn touch(&mut self, kind: BrowserMusic) {
        if kind == BrowserMusic::None {
            return;
        }
        self.kind = kind;
        self.last_seen = Some(Instant::now());
        self.checked_at = Some(Instant::now());
    }

    fn refresh(&mut self) {
        if self.checked_at.map(|t| t.elapsed() < CHECK_INTERVAL).unwrap_or(false) {
            return;
        }
        self.checked_at = Some(Instant::now());
        let mut list: Vec<(String, BrowserMusic)> = Vec::new();
        unsafe extern "system" fn callback(hwnd: HWND, data: LPARAM) -> BOOL {
            let list = &mut *(data.0 as *mut Vec<(String, BrowserMusic)>);
            if !IsWindowVisible(hwnd).as_bool() {
                return true.into();
            }
            let title = win::window_title(hwnd);
            let kind = classify_title(&title);
            if kind != BrowserMusic::None && is_browser_window(hwnd) {
                list.push((title, kind));
            }
            true.into()
        }
        unsafe {
            let _ = EnumWindows(Some(callback), LPARAM(&mut list as *mut _ as isize));
        }
        let found = list.iter().map(|(_, k)| *k).max_by_key(|k| rank(*k)).unwrap_or_default();
        self.windows = list;
        if found != BrowserMusic::None {
            self.kind = found;
            self.last_seen = Some(Instant::now());
        }
    }
}

fn is_browser_window(hwnd: HWND) -> bool {
    let pid = win::window_pid(hwnd);
    if pid == 0 || pid == win::own_pid() {
        return false;
    }
    match win::process_path(pid) {
        Some(path) => {
            let exe = win::file_stem(&path).to_lowercase();
            BROWSER_EXES.contains(&exe.as_str())
        }
        None => false,
    }
}
