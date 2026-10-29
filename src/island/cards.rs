//! Карточки острова: результат инструмента, напоминание, уведомление, вопрос агента, ответ ИИ.
//! Обычная карточка исчезает сама, «ждущая» (напоминание, вопрос агента) — только после ответа.

use std::path::PathBuf;
use std::rc::Rc;

use crate::gfx::Rgba;
use crate::i18n::{tr, trs};

pub const ORANGE: Rgba = Rgba::rgb(0xFF, 0x9F, 0x0A);
pub const GREEN: Rgba = Rgba::rgb(0x34, 0xC7, 0x59);
pub const RED: Rgba = Rgba::rgb(0xFF, 0x45, 0x3A);
pub const BLUE: Rgba = Rgba::rgb(0x0A, 0x84, 0xFF);
pub const PURPLE: Rgba = Rgba::rgb(0xBF, 0x5A, 0xF2);

/// Что делает кнопка или щелчок по карточке.
#[derive(Clone, Debug)]
pub enum Act {
    Copy(String),
    OpenUrl(String),
    OpenPath(PathBuf),
    ShowInFolder(PathBuf),
    Upload(PathBuf),
    ShowText(String, String),
    ReminderDone,
    Snooze(String, String, u64),
    Agent(u64, String),
    OpenApp(String),
    TimerDone,
}

#[derive(Clone)]
pub struct Card {
    pub glyph: char,
    pub tint: Rgba,
    pub title: String,
    pub body: String,
    /// Миниатюра скриншота.
    pub thumb: Option<Rc<tiny_skia::Pixmap>>,
    pub swatch: Option<Rgba>,
    pub buttons: Vec<(String, Act)>,
    pub click: Option<Act>,
    /// Ждёт ответа: сама не исчезает.
    pub sticky: bool,
    pub sound: bool,
    pub seconds: f32,
    /// Вопрос агента: закрывается, когда агент перестал ждать.
    pub agent: Option<u64>,
    pub busy: bool,
    /// Попадает в ленту на третьей вкладке.
    pub feed: bool,
}

impl Card {
    fn new(glyph: char, tint: Rgba, title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            glyph,
            tint,
            title: title.into(),
            body: body.into(),
            thumb: None,
            swatch: None,
            buttons: Vec::new(),
            click: None,
            sticky: false,
            sound: false,
            seconds: 6.0,
            agent: None,
            busy: false,
            feed: false,
        }
    }

    /// Работа идёт (распознавание, загрузка): держится, пока не придёт результат.
    pub fn busy(title: &str) -> Self {
        let mut c = Self::new('\u{E895}', BLUE, title, "");
        c.busy = true;
        c.seconds = 60.0;
        c
    }

    pub fn text(title: &str, body: String) -> Self {
        let mut c = Self::new('\u{E8C8}', GREEN, title, body.clone());
        let trimmed = body.trim();
        if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
            c.buttons.push((tr("Открыть", "Open").into(), Act::OpenUrl(trimmed.to_string())));
        }
        if body.lines().count() > 4 || body.chars().count() > 220 {
            c.buttons.push((tr("Целиком", "Full text").into(), Act::ShowText(title.to_string(), body.clone())));
        }
        c.buttons.push((tr("Копировать", "Copy").into(), Act::Copy(body)));
        c.seconds = 8.0;
        c
    }

    pub fn link(title: &str, url: String) -> Self {
        let mut c = Self::new('\u{E71B}', BLUE, title, url.clone());
        c.buttons.push((tr("Открыть", "Open").into(), Act::OpenUrl(url)));
        c
    }

    pub fn error(title: &str, body: String) -> Self {
        let mut c = Self::new('\u{E783}', RED, title, body);
        c.seconds = 8.0;
        c
    }

    pub fn file(title: &str, name: String, path: PathBuf) -> Self {
        let mut c = Self::new('\u{E714}', RED, title, name);
        c.buttons.push((tr("Папка", "Folder").into(), Act::ShowInFolder(path.clone())));
        c.buttons.push((tr("Открыть", "Open").into(), Act::OpenPath(path.clone())));
        c.click = Some(Act::OpenPath(path));
        c.seconds = 10.0;
        c
    }

    pub fn screenshot(shot: &crate::tools::capture::Shot, path: Option<PathBuf>, copied: bool) -> Self {
        let title = if copied { tr("Скриншот скопирован", "Screenshot copied") } else { tr("Скриншот сохранён", "Screenshot saved") };
        let body = format!("{} × {}", shot.w, shot.h);
        let mut c = Self::new('\u{E722}', BLUE, title, body);
        // Миниатюра: ужатая копия, чтобы не держать весь снимок
        let rgba = shot.to_rgba();
        let thumb_w = 240u32.min(shot.w);
        let thumb_h = ((shot.h as f32 * thumb_w as f32 / shot.w.max(1) as f32) as u32).max(1);
        let small = image::imageops::thumbnail(&rgba, thumb_w, thumb_h);
        c.thumb = Some(Rc::new(crate::gfx::rgba_to_pixmap(&small)));
        if let Some(p) = path {
            c.buttons.push((tr("Ссылка", "Link").into(), Act::Upload(p.clone())));
            c.buttons.push((tr("Папка", "Folder").into(), Act::ShowInFolder(p.clone())));
            c.click = Some(Act::OpenPath(p));
        }
        c.seconds = 7.0;
        c
    }

    pub fn color(rgb: [u8; 3], hex: String) -> Self {
        let mut c = Self::new('\u{EF3C}', Rgba::rgb(rgb[0], rgb[1], rgb[2]), hex.clone(), trs(format!("RGB {}, {}, {} · скопирован", rgb[0], rgb[1], rgb[2]), format!("RGB {}, {}, {} · copied", rgb[0], rgb[1], rgb[2])));
        c.swatch = Some(Rgba::rgb(rgb[0], rgb[1], rgb[2]));
        c.buttons.push(("RGB".into(), Act::Copy(format!("{}, {}, {}", rgb[0], rgb[1], rgb[2]))));
        c.buttons.push(("HEX".into(), Act::Copy(hex)));
        c.seconds = 5.0;
        c
    }

    pub fn reminder(id: &str, text: &str) -> Self {
        let mut c = Self::new('\u{E823}', ORANGE, tr("Напоминание", "Reminder"), text);
        c.sticky = true;
        c.sound = true;
        c.feed = true;
        c.buttons.push((tr("Через 10 мин", "In 10 min").into(), Act::Snooze(id.to_string(), text.to_string(), 10)));
        c.buttons.push((tr("Готово", "Done").into(), Act::ReminderDone));
        c
    }

    pub fn timer_done() -> Self {
        let mut c = Self::new('\u{E916}', ORANGE, tr("Время вышло", "Time is up"), tr("Таймер закончился", "The timer has finished"));
        c.sticky = true;
        c.sound = true;
        c.buttons.push(("OK".into(), Act::TimerDone));
        c
    }

    pub fn notification(t: crate::notify::Toast) -> Self {
        let title = if t.title.is_empty() { t.app.clone() } else { format!("{} · {}", t.app, t.title) };
        let mut c = Self::new('\u{EA8F}', BLUE, title, t.body.clone());
        c.click = Some(Act::OpenApp(t.aumid.clone()));
        c.feed = true;
        c.seconds = 7.0;
        c
    }

    pub fn agent(name: &str, text: String) -> Self {
        let mut c = Self::new('\u{E99A}', PURPLE, name, text.clone());
        c.feed = true;
        c.seconds = 9.0;
        if text.chars().count() > 220 || text.lines().count() > 4 {
            c.buttons.push((tr("Целиком", "Full text").into(), Act::ShowText(name.to_string(), text)));
        }
        c
    }

    /// Агент ждёт решения: разрешить или запретить, либо один из вариантов.
    pub fn agent_question(id: u64, title: String, body: String, options: Vec<String>) -> Self {
        let mut c = Self::new('\u{E99A}', ORANGE, title, body);
        c.sticky = true;
        c.sound = true;
        c.agent = Some(id);
        c.feed = true;
        if options.is_empty() {
            c.buttons.push((tr("Запретить", "Deny").into(), Act::Agent(id, "deny".into())));
            c.buttons.push((tr("Разрешить", "Allow").into(), Act::Agent(id, "allow".into())));
        } else {
            for o in options.into_iter().take(4) {
                c.buttons.push((o.clone(), Act::Agent(id, format!("option:{o}"))));
            }
        }
        c
    }

    pub fn ai(question: String, answer: String) -> Self {
        let mut c = Self::new('\u{E99A}', PURPLE, question.clone(), answer.clone());
        c.seconds = 20.0;
        c.feed = true;
        c.buttons.push((tr("Целиком", "Full text").into(), Act::ShowText(question, answer.clone())));
        c.buttons.push((tr("Копировать", "Copy").into(), Act::Copy(answer)));
        c
    }
}
