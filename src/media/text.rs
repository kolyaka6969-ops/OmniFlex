//! Разбор названий песен и роликов: очистка от «(Official Video)» и названия сайта, исполнитель из названия,
//! варианты прочтения для поиска, сравнение найденного с искомым.

use regex::Regex;
use std::sync::OnceLock;

use super::MusicSource;
use crate::util::{strip_suffix_ignore_case, trim_chars};

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("regex"))
}

/// Шум в названиях роликов YouTube: «(Official Video)», «[MV]», «(Lyrics)» и подобное.
const VIDEO_NOISE: &[&str] = &[
    "official", "video", "audio", "lyric", "клип", "премьер", "premiere", "официальн", "видео", "текст", "visualizer",
    "hd", "4k", "8k", "m/v", "mv", "kbps", "320", "slowed", "reverb", "sped up", "speed up", "bass boost", "nightcore",
    "full", "creditless", "fps", "1080", "tv size", "romaji", "eng sub",
];

fn brackets() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"[\(\[\{][^\)\]\}]*[\)\]\}]")
}

fn noise_tail() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)\s*[+|/•–—-]\s*(?:текст(?:\s+песни)?|с\s+текстом|lyrics?|караоке|karaoke)\s*$")
}

fn feat_tail() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"(?i)\s+(?:ft|feat|featuring|starring)\.?\s.*$")
}

pub fn multi_space() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"\s{2,}")
}

const DASH_TRIM: &[char] = &[' ', '-', '—', '–', '|'];
const SEPARATORS: [&str; 4] = [" — ", " – ", " - ", " | "];

/// Сравниваем без регистра, знаков и «ё»: «Ёлка» и «елка» — одно и то же.
pub fn norm(text: Option<&str>) -> String {
    let Some(text) = text else { return String::new() };
    if text.trim().is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(text.len());
    for ch in text.to_lowercase().chars() {
        if ch == 'ё' {
            out.push('е');
        } else if ch.is_alphanumeric() {
            out.push(ch);
        } else if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim().to_string()
}

/// Насколько найденный вариант похож на искомую песню. Ноль — это другая песня.
pub fn score(title: &str, artist: Option<&str>, candidate_title: Option<&str>, candidate_artist: Option<&str>) -> i32 {
    let wanted = norm(Some(title));
    let found = norm(candidate_title);
    if wanted.chars().count() < 2 || found.chars().count() < 2 {
        return 0;
    }
    let wanted_ns = wanted.replace(' ', "");
    let found_ns = found.replace(' ', "");
    let mut score = if wanted == found || (wanted_ns.chars().count() >= 2 && wanted_ns == found_ns) {
        3
    } else if wanted.chars().count() >= 5
        && (wanted.contains(&found)
            || found.contains(&wanted)
            || (wanted_ns.chars().count() >= 5 && (wanted_ns.contains(&found_ns) || found_ns.contains(&wanted_ns))))
    {
        2
    } else {
        return 0;
    };

    let wanted_artist = norm(artist);
    let found_artist = norm(candidate_artist);
    if !wanted_artist.is_empty() && !found_artist.is_empty() {
        let wa_ns = wanted_artist.replace(' ', "");
        let fa_ns = found_artist.replace(' ', "");
        if wanted_artist == found_artist || (wa_ns.chars().count() >= 2 && wa_ns == fa_ns) {
            score += 3;
        } else if wanted_artist.contains(&found_artist)
            || found_artist.contains(&wanted_artist)
            || (wa_ns.chars().count() >= 3 && (wa_ns.contains(&fa_ns) || fa_ns.contains(&wa_ns)))
        {
            score += 2;
        }
    }
    score
}

pub fn without_site_name(title: &str) -> String {
    let mut title = title.trim().to_string();
    for suffix in [
        " — Яндекс Музыка",
        " | Яндекс Музыка",
        " - Яндекс Музыка",
        " — Yandex Music",
        " | Yandex Music",
        " - YouTube",
        " | YouTube",
    ] {
        if let Some(stripped) = strip_suffix_ignore_case(&title, suffix) {
            title = stripped.trim().to_string();
        }
    }
    title
}

/// «ПОШЛАЯ МОЛЛИ –СУПЕРМАРКЕТ» → «ПОШЛАЯ МОЛЛИ – СУПЕРМАРКЕТ»: на YouTube пробел у тире часто забывают,
/// и название не делилось на исполнителя и песню. Тире без пробелов вовсе («Jay-Z», «Hip-Hop») не трогаем.
pub fn space_dashes(title: &str) -> String {
    let chars: Vec<char> = title.chars().collect();
    let mut out = String::with_capacity(title.len() + 4);
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Пробелы перед тире без пробела после: «А –Б»
        if c.is_whitespace() && i > 0 && !chars[i - 1].is_whitespace() {
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j + 1 < chars.len() && is_dash(chars[j]) && !chars[j + 1].is_whitespace() {
                out.push(' ');
                out.push(chars[j]);
                out.push(' ');
                i = j + 1;
                continue;
            }
        }
        // Тире сразу после слова и пробелы после: «А– Б»
        if is_dash(c) && i > 0 && !chars[i - 1].is_whitespace() && i + 1 < chars.len() && chars[i + 1].is_whitespace() {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() {
                out.push(' ');
                out.push(c);
                out.push(' ');
                i = j;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

fn is_dash(c: char) -> bool {
    matches!(c, '-' | '–' | '—')
}

fn has_video_noise(bracket: &str) -> bool {
    let lower = bracket.to_lowercase();
    VIDEO_NOISE.iter().any(|w| lower.contains(w))
}

/// «Кино - Группа крови (Official Video)» → «Кино - Группа крови».
pub fn strip_video_noise(title: &str) -> String {
    let cleaned = brackets().replace_all(title, |caps: &regex::Captures| {
        let m = caps.get(0).unwrap().as_str();
        if has_video_noise(m) {
            " ".to_string()
        } else {
            m.to_string()
        }
    });
    let cleaned = noise_tail().replace_all(&cleaned, "");
    let cleaned = multi_space().replace_all(&cleaned, " ");
    let cleaned = trim_chars(&cleaned, &[' ', '-', '—', '–', '|', '+']).to_string();
    if cleaned.is_empty() {
        title.trim().to_string()
    } else {
        cleaned
    }
}

/// Название ролика YouTube без названия сайта и пометок вроде «(Official Video)».
pub fn video_title(title: &str) -> String {
    strip_video_noise(&without_site_name(title))
}

/// Канал YouTube — не всегда исполнитель: у «Кино - Topic» и «KinoVEVO» имя трека в конце.
pub fn clean_channel(artist: Option<&str>) -> Option<String> {
    let artist = artist?;
    if artist.trim().is_empty() {
        return None;
    }
    let mut name = artist.trim().to_string();
    if name.eq_ignore_ascii_case("Topic") {
        return None;
    }
    let mut stripped = false;
    for suffix in [" - Topic", " – Topic", " — Topic", "VEVO", " Official", " Official Channel"] {
        if name.chars().count() > suffix.chars().count() {
            if let Some(s) = strip_suffix_ignore_case(&name, suffix) {
                name = s.trim().to_string();
                stripped = true;
            }
        }
    }
    // «TheCranberriesVEVO» → «TheCranberries» → «The Cranberries»
    if stripped && !name.contains(' ') {
        let chars: Vec<char> = name.chars().collect();
        let mut out = String::new();
        for (i, c) in chars.iter().enumerate() {
            if i > 0 && c.is_uppercase() && (chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit()) {
                out.push(' ');
            }
            out.push(*c);
        }
        name = out;
    }
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Браузеры присылают «Исполнитель — Название» одной строкой, часто с названием сайта в конце.
/// У YouTube в названии ещё и «(Official Video)», а в исполнителе — канал. Приводим к виду,
/// который понимают базы текстов и экран на рабочем столе.
pub fn clean_metadata(
    title: &str,
    artist: Option<&str>,
    album: Option<&str>,
    source: MusicSource,
) -> (String, Option<String>, Option<String>) {
    let mut title = without_site_name(title);
    if source != MusicSource::Spotify {
        title = space_dashes(&title);
    }
    let mut artist = artist.map(|s| s.to_string());
    let album = album.map(|s| s.to_string());

    if matches!(source, MusicSource::YouTube | MusicSource::YouTubeMusic) {
        title = strip_video_noise(&title);
        // Опенинг аниме: песня и исполнитель внутри названия, а канал — просто тот, кто залил
        if source == MusicSource::YouTube {
            if let Some((t, a)) = super::anime::from_title(&title) {
                return (t, a, album);
            }
        }
        artist = clean_channel(artist.as_deref());
    }

    if artist.as_deref().map(|a| a.trim().is_empty()).unwrap_or(true) && source != MusicSource::Spotify {
        for sep in SEPARATORS {
            if let Some(i) = title.find(sep) {
                if i > 0 && i + sep.len() < title.len() {
                    artist = Some(title[..i].trim().to_string());
                    title = title[i + sep.len()..].trim().to_string();
                    break;
                }
            }
        }
    }

    // На YouTube исполнитель нередко уже в названии: «Кино - Группа крови» при канале «Кино - Topic».
    if matches!(source, MusicSource::YouTube | MusicSource::YouTubeMusic) {
        if let Some(a) = artist.clone().filter(|a| !a.trim().is_empty()) {
            let mut matched = false;
            for sep in SEPARATORS {
                let prefix = format!("{a}{sep}");
                if crate::util::starts_with_ignore_case(&title, &prefix) && title.len() > prefix.len() {
                    title = title[prefix.len()..].trim().to_string();
                    matched = true;
                    break;
                }
                // Канал «Wiz Khalifa Music», «Мот TV»: имя канала начинается с исполнителя из названия
                if let Some(i) = title.find(sep) {
                    if title[..i].chars().count() >= 2 && i + sep.len() < title.len() {
                        let left = title[..i].trim().to_string();
                        if a.chars().count() > left.chars().count()
                            && crate::util::starts_with_ignore_case(&a, &format!("{left} "))
                        {
                            artist = Some(left);
                            title = title[i + sep.len()..].trim().to_string();
                            matched = true;
                            break;
                        }
                    }
                }
            }
            // Канал не совпал с исполнителем, а в названии ролика чётко «Исполнитель - Название песни»
            if !matched {
                for sep in SEPARATORS {
                    if let Some(i) = title.find(sep) {
                        if title[..i].chars().count() >= 2 && i + sep.len() < title.len() {
                            let left = title[..i].trim().to_string();
                            let right = title[i + sep.len()..].trim().to_string();
                            if left.chars().count() >= 2 && right.chars().count() >= 2 {
                                artist = Some(left);
                                title = right;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    let artist = artist.filter(|a| !a.trim().is_empty());
    (title, artist, album)
}

/// Без приглашённого исполнителя и всего, что после него: «Песня ft. Гость …» → «Песня».
pub fn without_feat(title: &str) -> String {
    let cleaned = feat_tail().replace(title, "");
    let cleaned = trim_chars(&cleaned, DASH_TRIM);
    if cleaned.chars().count() < 2 {
        title.to_string()
    } else {
        cleaned.to_string()
    }
}

/// Название без пояснений в скобках: «Песня (Official Video)» → «Песня».
pub fn without_brackets(title: &str) -> String {
    let cleaned = brackets().replace_all(title, " ");
    let cleaned = multi_space().replace_all(&cleaned, " ");
    let cleaned = trim_chars(&cleaned, DASH_TRIM);
    if cleaned.is_empty() {
        title.trim().to_string()
    } else {
        cleaned.to_string()
    }
}

/// Варианты пары «название — исполнитель» для поиска в каталогах. У YouTube в названии часто
/// «Песня - Исполнитель (пояснение)», а в поле исполнителя — канал, поэтому одного варианта мало:
/// перебираем прочтения, начиная с самого вероятного.
pub fn candidates(title: &str, artist: Option<&str>) -> Vec<(String, Option<String>)> {
    let mut list: Vec<(String, Option<String>)> = Vec::new();
    let mut add = |t: Option<&str>, a: Option<&str>| {
        let Some(t) = t else { return };
        let t = trim_chars(t.trim(), DASH_TRIM).to_string();
        let a = clean_channel(a);
        if t.trim().is_empty() || t.chars().count() < 2 {
            return;
        }
        let exists = list.iter().any(|(lt, la)| {
            lt.to_lowercase() == t.to_lowercase()
                && la.as_deref().unwrap_or("").to_lowercase() == a.as_deref().unwrap_or("").to_lowercase()
        });
        if !exists {
            list.push((t, a));
        }
    };

    let clean_artist = clean_channel(artist);
    add(Some(title), clean_artist.as_deref());
    // Скобки — обычно пояснение к ролику, а не часть названия песни
    let stripped = without_brackets(title);
    // «See You Again ft. Charlie Puth Furious 7 Soundtrack» → «See You Again»: в каталогах гость не в названии
    let core = without_feat(&stripped);
    if core != stripped {
        if clean_artist.is_some() {
            add(Some(&core), clean_artist.as_deref());
        }
        split(&core, &mut add);
    }
    split(&stripped, &mut add);
    add(Some(&stripped), clean_artist.as_deref());
    add(Some(&stripped), None);
    split(title, &mut add);
    list
}

fn split(title: &str, add: &mut impl FnMut(Option<&str>, Option<&str>)) {
    for sep in [" — ", " – ", " - ", " | ", ":"] {
        if let Some(i) = title.find(sep) {
            if i == 0 || i + sep.len() >= title.len() {
                continue;
            }
            let left = title[..i].trim();
            let right = title[i + sep.len()..].trim();
            // В роликах «Песня - Исполнитель», а после двоеточия наоборот: «Исполнитель: Песня»
            if sep == ":" {
                add(Some(right), Some(left));
                add(Some(left), Some(right));
            } else {
                add(Some(left), Some(right));
                add(Some(right), Some(left));
            }
            return;
        }
    }
}

/// Пометки, которые ставят только у музыки: у влогов и лекций их не бывает.
const MUSIC_VIDEO_MARKS: &[&str] = &[
    "клип", "clip", "fan made", "fanmade", "fan video", "amv", "official video", "official music video", "music video",
    "official audio", "lyric video", "lyrics", "текст песни", "mv", "m/v", "cover", "кавер", "remix", "ремикс",
    "visualizer", "slowed", "sped up", "nightcore", "премьера трека", "премьера песни", "kbps",
];

/// Название ролика похоже на песню: «Исполнитель - Песня (Fan Made Clip)», «Песня [кавер]».
pub fn is_music_video(title: &str) -> bool {
    let lower = title.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    MUSIC_VIDEO_MARKS.iter().any(|mark| {
        let mark_chars: Vec<char> = mark.chars().collect();
        if mark_chars.len() > chars.len() {
            return false;
        }
        (0..=chars.len() - mark_chars.len()).any(|i| {
            chars[i..i + mark_chars.len()] == mark_chars[..]
                && (i == 0 || !chars[i - 1].is_alphanumeric())
                && (i + mark_chars.len() == chars.len() || !chars[i + mark_chars.len()].is_alphanumeric())
        })
    })
}

/// Есть ли в тексте кириллические буквы.
pub fn has_cyrillic(text: Option<&str>) -> bool {
    text.map(|t| t.chars().any(|c| ('Ѐ'..='ӿ').contains(&c))).unwrap_or(false)
}
