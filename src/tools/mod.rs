//! Инструменты острова: скриншоты с разметкой, текст и QR-код с экрана, пипетка, запись экрана и вставка
//! без форматирования. Окна инструментов живут в потоке окон; долгая работа (распознавание, загрузка) —
//! в своих потоках, результат приходит карточкой на остров.

pub mod capture;
pub mod clipboard;
pub mod ocr;
pub mod overlay;
pub mod qr;
pub mod record;
pub mod upload;

use std::path::PathBuf;

use windows::core::GUID;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Shell::{SHGetKnownFolderPath, KF_FLAG_DEFAULT};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::i18n::{tr, trs};
use crate::island::{self, Card};
use crate::settings;
use capture::Shot;
use overlay::Purpose;

pub fn known_folder(id: &GUID) -> PathBuf {
    unsafe {
        match SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
                PathBuf::from(s)
            }
            Err(_) => std::env::temp_dir(),
        }
    }
}

pub fn screenshot_folder() -> PathBuf {
    let custom = settings::read(|s| s.tools.screenshot_dir.clone());
    if !custom.trim().is_empty() {
        return PathBuf::from(custom);
    }
    known_folder(&windows::Win32::UI::Shell::FOLDERID_Pictures).join("OmniFlex")
}

pub fn screenshot_region() {
    overlay::start(Purpose::Screenshot);
}

/// Весь экран, на котором курсор.
pub fn screenshot_full() {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    let m = crate::win::monitor_at(pt);
    if let Some(shot) = capture::grab(m.bounds, false) {
        screenshot_taken(shot);
    }
}

/// Скриншот готов: в буфер и в папку, как в настройках, и карточкой на остров.
pub fn screenshot_taken(shot: Shot) {
    let (copy, save) = settings::read(|s| (s.tools.copy_screenshots, s.tools.save_screenshots));
    if copy {
        clipboard::set_image(&shot);
    }
    let mut path = None;
    if save || !copy {
        let dir = screenshot_folder();
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join(chrono::Local::now().format("OmniFlex %Y-%m-%d %H-%M-%S.png").to_string());
        if std::fs::write(&p, shot.png()).is_ok() {
            path = Some(p);
        }
    }
    island::card(Card::screenshot(&shot, path, copy));
}

pub fn screen_text() {
    overlay::start(Purpose::Region(Box::new(|_, shot| {
        island::card(Card::busy(tr("Распознаю текст…", "Recognizing text…")));
        std::thread::spawn(move || {
            unsafe {
                let _ = windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED);
            }
            let result = ocr::recognize_text(&shot);
            crate::shell::post(move || match result {
                Ok(text) => {
                    clipboard::set_text(&text);
                    island::card(Card::text(tr("Текст скопирован", "Text copied"), text));
                }
                Err(e) => island::card(Card::error(tr("Текст с экрана", "Screen text"), e)),
            });
        });
    })));
}

pub fn qr_code() {
    overlay::start(Purpose::Region(Box::new(|_, shot| {
        std::thread::spawn(move || {
            let found = qr::decode(&shot);
            crate::shell::post(move || {
                if found.is_empty() {
                    island::card(Card::error(tr("QR-код", "QR code"), tr("В выделенной области QR-кода нет", "No QR code in the selected area").into()));
                    return;
                }
                let text = found.join("\n");
                clipboard::set_text(&text);
                island::card(Card::text(tr("QR-код скопирован", "QR code copied"), text));
            });
        });
    })));
}

pub fn color_picker() {
    overlay::start(Purpose::Picker);
}

pub fn color_picked(rgb: [u8; 3]) {
    let hex = format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2]);
    clipboard::set_text(&hex);
    let h = hex.clone();
    settings::update(move |s| {
        s.tools.colors.retain(|c| c != &h);
        s.tools.colors.insert(0, h);
        s.tools.colors.truncate(12);
    });
    island::card(Card::color(rgb, hex));
}

pub fn toggle_record() {
    if record::is_recording() {
        record::stop();
        return;
    }
    overlay::start(Purpose::Region(Box::new(|rect, _| record::start(rect))));
}

pub fn paste_plain() {
    if !clipboard::paste_plain() {
        island::card(Card::error(tr("Чистый текст", "Plain text"), tr("В буфере нет текста", "No text in the clipboard").into()));
    }
}

/// Загрузить скриншот и скопировать ссылку.
pub fn upload(path: PathBuf) {
    if !upload::configured() {
        island::card(Card::error(
            tr("Ссылка на скриншот", "Screenshot link"),
            tr("Выберите Imgur или ImgBB и укажите ключ в настройках инструментов", "Choose Imgur or ImgBB and enter a key in tool settings").into(),
        ));
        return;
    }
    let Ok(png) = std::fs::read(&path) else { return };
    island::card(Card::busy(tr("Загружаю…", "Uploading…")));
    crate::media::runtime().spawn(async move {
        let result = upload::upload(png).await;
        crate::shell::post(move || match result {
            Ok(link) => {
                clipboard::set_text(&link);
                island::card(Card::link(tr("Ссылка скопирована", "Link copied"), link));
            }
            Err(e) => island::card(Card::error(tr("Загрузка не удалась", "Upload failed"), trs(e.clone(), e))),
        });
    });
}
