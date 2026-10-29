//! Напоминания: разовые (на дату) и повторяющиеся по дням недели. Срабатывают карточкой со звуком на острове
//! и ждут ответа: «Готово» или «Через 10 минут». Проверяются потоком окон каждые несколько секунд.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use chrono::{Datelike, Local, NaiveDate, NaiveTime, Timelike};

use crate::i18n::tr;
use crate::settings::{self, Reminder};

thread_local! {
    /// Отложенные: когда, текст, id.
    static SNOOZED: RefCell<Vec<(Instant, String, String)>> = const { RefCell::new(Vec::new()) };
}

pub fn parse_time(s: &str) -> Option<NaiveTime> {
    let s = s.trim().replace('.', ":");
    NaiveTime::parse_from_str(&s, "%H:%M").ok().or_else(|| s.parse::<u32>().ok().filter(|h| *h < 24).and_then(|h| NaiveTime::from_hms_opt(h, 0, 0)))
}

/// Дни недели из текста: «пн, ср, пт», «будни», «выходные», «каждый день», «mon wed», «weekdays».
pub fn parse_days(text: &str) -> Vec<u8> {
    let t = text.to_lowercase();
    if t.contains("кажд") || t.contains("ежедн") || t.contains("every day") || t.contains("daily") {
        return (0..7).collect();
    }
    let mut days = Vec::new();
    if t.contains("будн") || t.contains("weekday") {
        days.extend(0..5);
    }
    if t.contains("выходн") || t.contains("weekend") {
        days.extend(5..7);
    }
    let names: [(&[&str], u8); 7] = [
        (&["пн", "понед", "mon"], 0),
        (&["вт", "вторн", "tue"], 1),
        (&["ср", "сред", "wed"], 2),
        (&["чт", "четв", "thu"], 3),
        (&["пт", "пятн", "fri"], 4),
        (&["сб", "субб", "sat"], 5),
        (&["вс", "воскр", "sun"], 6),
    ];
    for word in t.split(|c: char| !c.is_alphanumeric()) {
        for (keys, day) in names.iter() {
            if keys.iter().any(|k| word.starts_with(k)) && !days.contains(day) {
                days.push(*day);
            }
        }
    }
    days.sort();
    days.dedup();
    days
}

pub fn days_label(days: &[u8]) -> String {
    if days.len() == 7 {
        return tr("каждый день", "every day").into();
    }
    if days == [0, 1, 2, 3, 4] {
        return tr("по будням", "on weekdays").into();
    }
    if days == [5, 6] {
        return tr("по выходным", "on weekends").into();
    }
    let ru = ["пн", "вт", "ср", "чт", "пт", "сб", "вс"];
    let en = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
    let names = if crate::i18n::is_english() { en } else { ru };
    days.iter().filter_map(|d| names.get(*d as usize)).cloned().collect::<Vec<_>>().join(", ")
}

/// Ближайшее срабатывание напоминания после now.
pub fn next_time(r: &Reminder, now: chrono::NaiveDateTime) -> Option<chrono::NaiveDateTime> {
    if !r.enabled {
        return None;
    }
    let time = parse_time(&r.time)?;
    if r.days.is_empty() {
        let date = NaiveDate::parse_from_str(&r.date, "%Y-%m-%d").ok()?;
        let at = date.and_time(time);
        return if at >= now - chrono::Duration::seconds(59) { Some(at) } else { None };
    }
    for offset in 0..8 {
        let date = now.date() + chrono::Duration::days(offset);
        let weekday = date.weekday().num_days_from_monday() as u8;
        if r.days.contains(&weekday) {
            let at = date.and_time(time);
            if at >= now - chrono::Duration::seconds(59) {
                return Some(at);
            }
        }
    }
    None
}

/// Ближайшие напоминания для острова.
pub fn upcoming(limit: usize) -> Vec<(chrono::NaiveDateTime, Reminder)> {
    let now = Local::now().naive_local();
    let mut list: Vec<_> = settings::read(|s| s.reminders.clone()).into_iter().filter_map(|r| next_time(&r, now).map(|t| (t, r))).collect();
    list.sort_by_key(|(t, _)| *t);
    list.truncate(limit);
    list
}

/// Проверка: что пора показать.
pub fn check() {
    let now = Local::now().naive_local();
    let stamp = now.format("%Y-%m-%d %H:%M").to_string();
    let due: Vec<Reminder> = settings::read(|s| {
        s.reminders
            .iter()
            .filter(|r| r.enabled && r.fired != stamp)
            .filter(|r| next_time(r, now).map(|t| t.date() == now.date() && t.hour() == now.hour() && t.minute() == now.minute()).unwrap_or(false))
            .cloned()
            .collect()
    });
    if !due.is_empty() {
        let ids: Vec<String> = due.iter().map(|r| r.id.clone()).collect();
        let stamp2 = stamp.clone();
        settings::update(|s| {
            for r in &mut s.reminders {
                if ids.contains(&r.id) {
                    r.fired = stamp2.clone();
                    // Разовое отработало — выключаем, чтобы не висело в списке ближайших
                    if r.days.is_empty() {
                        r.enabled = false;
                    }
                }
            }
        });
        for r in due {
            crate::island::card(crate::island::Card::reminder(&r.id, &r.text));
        }
    }
    let ready: Vec<(String, String)> = SNOOZED.with(|s| {
        let mut s = s.borrow_mut();
        let now = Instant::now();
        let ready = s.iter().filter(|(t, _, _)| *t <= now).map(|(_, text, id)| (id.clone(), text.clone())).collect();
        s.retain(|(t, _, _)| *t > now);
        ready
    });
    for (id, text) in ready {
        crate::island::card(crate::island::Card::reminder(&id, &text));
    }
}

pub fn snooze(id: &str, text: &str, minutes: u64) {
    let (id, text) = (id.to_string(), text.to_string());
    SNOOZED.with(|s| s.borrow_mut().push((Instant::now() + Duration::from_secs(minutes * 60), text, id)));
}

/// Звук напоминания или таймера.
pub fn play_sound() {
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_NODEFAULT};
    let alias = crate::util::wide("Notification.Reminder");
    let fallback = crate::util::wide("SystemAsterisk");
    unsafe {
        if !PlaySoundW(windows::core::PCWSTR(alias.as_ptr()), None, SND_ALIAS | SND_ASYNC | SND_NODEFAULT).as_bool() {
            let _ = PlaySoundW(windows::core::PCWSTR(fallback.as_ptr()), None, SND_ALIAS | SND_ASYNC);
        }
    }
}

/// Быстрое разовое напоминание через n минут (из острова).
pub fn add_in(minutes: i64, text: &str) {
    let at = Local::now().naive_local() + chrono::Duration::minutes(minutes);
    let r = Reminder {
        id: settings::new_id(),
        text: text.to_string(),
        time: at.format("%H:%M").to_string(),
        date: at.format("%Y-%m-%d").to_string(),
        days: Vec::new(),
        enabled: true,
        fired: String::new(),
    };
    settings::update(|s| s.reminders.push(r));
}
