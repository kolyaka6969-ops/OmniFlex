//! Опенинги и эндинги аниме с YouTube. В названии ролика обычно аниме и номер опенинга, а песня и исполнитель —
//! если повезёт: «Chainsaw Man - Opening | KICK BACK», «Jujutsu Kaisen - Opening 2 | 4K». Недостающее берём
//! из открытой базы AnimeThemes: по аниме и номеру она знает песню и исполнителя.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use fancy_regex::Regex as FRegex;
use futures::future::{FutureExt, Shared};
use regex::Regex;

use super::text::norm;
use super::{escape, slow, str_of};

pub type Song = (String, Option<String>);

/// Что удалось прочитать из названия ролика. kind — «OP» или «ED».
#[derive(Clone, Debug)]
pub struct Parsed {
    pub anime: Option<String>,
    pub kind: &'static str,
    pub number: Option<i32>,
    pub song: Option<String>,
    pub artist: Option<String>,
    pub strong: bool,
}

const CACHE_LIMIT: usize = 200;
const RETRY_AFTER: Duration = Duration::from_secs(60);

fn f(cell: &'static OnceLock<FRegex>, p: &str) -> &'static FRegex {
    cell.get_or_init(|| FRegex::new(p).expect("regex"))
}
fn r(cell: &'static OnceLock<Regex>, p: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(p).expect("regex"))
}

// Латинские OP и ED — только заглавными: «Ed Sheeran» не эндинг. Группы 1 и 3 — опенинг, 2 и 4 — эндинг
fn marker() -> &'static FRegex {
    static R: OnceLock<FRegex> = OnceLock::new();
    f(
        &R,
        r"(?<![A-Za-z\d\p{Cyrillic}])(?:((?i:opening|опенинг)|NCOP|OP)|((?i:ending|эндинг)|NCED|ED))(?![\p{L}])|(オープニング|ＯＰ)|(エンディング|ＥＤ)",
    )
}
fn number() -> &'static FRegex {
    static R: OnceLock<FRegex> = OnceLock::new();
    f(&R, r"^\s*(\d{1,2})(?!\d)")
}
/// Приметы ролика с аниме: без них «Happy Ending» осталось бы просто песней.
fn anime_hint() -> &'static FRegex {
    static R: OnceLock<FRegex> = OnceLock::new();
    f(
        &R,
        r"(?i)(?<![\p{L}])(?:full|tv\s*size|creditless|non-?credit|anime|аниме|season|сезон)(?![\p{L}])|アニメ|ノンクレジット|主題歌",
    )
}
fn quote() -> &'static FRegex {
    static R: OnceLock<FRegex> = OnceLock::new();
    f(
        &R,
        r#"「([^」]+)」|『([^』]+)』|“([^”]+)”|"([^"]+)"|«([^»]+)»|(?<![\p{L}\d])'([^']+)'(?![\p{L}\d])"#,
    )
}
fn anime_before_quote() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    r(&R, r"(?:アニメ|劇場版)\s*$")
}
fn separator() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    r(&R, r"\s*[｜│|]\s*|\s+[-–—/]\s+")
}
fn by_artist() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    r(&R, r"^(?P<song>.+?)\s+(?i:by)\s+(?P<artist>.+)$")
}
fn after_quote_artist() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    r(&R, r"^\s*(?i:by)\s+(?P<artist>[^|｜│\[(【]+?)\s*(?:[|｜│\[(【]|\s[-–—/]\s|$)")
}
/// Сегмент без песни: «4K», «60FPS», «Creditless», «Full», «TV Size».
fn noise_segment() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    r(
        &R,
        r"^(?i:\s|[,&+.]|\d+k|\d+\s*fps|\d{3,4}p|u?hd|creditless|full|tv\s*size|lyrics?|romaji|(?:eng(?:lish)?\s*)?sub(?:s|bed)?|amv|mad|ncop\d*|nced\d*|official|version|ver)*$",
    )
}
fn season_word() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    r(&R, r"(?i)^(?:season|part|cour|\d+(?:st|nd|rd|th)?)$")
}

#[derive(Clone, Debug)]
struct QuoteMatch {
    start: usize,
    end: usize,
    text: String,
}

fn quotes(s: &str) -> Vec<QuoteMatch> {
    let mut out = Vec::new();
    for caps in quote().captures_iter(s).flatten() {
        let whole = caps.get(0).unwrap();
        let text = (1..=6).find_map(|i| caps.get(i)).map(|m| m.as_str().to_string()).unwrap_or_default();
        out.push(QuoteMatch { start: whole.start(), end: whole.end(), text });
    }
    out
}

#[derive(Clone, Debug)]
struct Segment {
    start: usize,
    text: String,
    raw_len: usize,
}

fn segments(title: &str) -> Vec<Segment> {
    let mut list = Vec::new();
    let mut start = 0;
    for sep in separator().find_iter(title) {
        list.push(Segment { start, text: title[start..sep.start()].trim().to_string(), raw_len: sep.start() - start });
        start = sep.end();
    }
    list.push(Segment { start, text: title[start..].trim().to_string(), raw_len: title.len() - start });
    list
}

fn has_letter(s: &str) -> bool {
    s.chars().any(|c| c.is_alphabetic())
}

/// Разбор названия без сети. None — не опенинг и не эндинг. strong — уверены, что это аниме.
pub fn parse(title: &str) -> Option<Parsed> {
    let caps = marker().captures(title).ok().flatten()?;
    let m = caps.get(0)?;
    let kind = if caps.get(2).is_some() || caps.get(4).is_some() { "ED" } else { "OP" };
    let marker_index = m.start();
    let mut marker_end = m.end();
    let mut num = None;
    if let Ok(Some(d)) = number().captures(&title[marker_end..]) {
        num = d.get(1).and_then(|x| x.as_str().parse::<i32>().ok());
        marker_end += d.get(0).unwrap().end();
    }
    // Английские «Opening» и «Ending» бывают и в названиях песен, остальные метки — нет
    let mv = m.as_str();
    let plain_word = (mv.eq_ignore_ascii_case("opening") || mv.eq_ignore_ascii_case("ending")) && mv.chars().any(|c| c.is_lowercase());
    let mut strong = num.is_some() || !plain_word || anime_hint().is_match(title).unwrap_or(false);

    let segs = segments(title);
    let k = segs.iter().position(|s| s.start <= marker_index && marker_index < s.start + s.raw_len);
    let mut song: Option<String> = None;
    let mut artist: Option<String> = None;
    let mut anime: Option<String> = None;

    let qs = quotes(title);
    let anime_quote = qs.iter().find(|q| q.start < marker_index && anime_before_quote().is_match(&title[..q.start])).cloned();
    let song_quote = qs
        .iter()
        .find(|q| q.start >= marker_end)
        .cloned()
        .or_else(|| qs.iter().rev().find(|q| q.start < marker_index && Some(q.start) != anime_quote.as_ref().map(|a| a.start)).cloned());
    if let Some(sq) = &song_quote {
        song = Some(sq.text.trim().to_string());
        if let Some(by) = after_quote_artist().captures(&title[sq.end..]) {
            artist = Some(by["artist"].trim().to_string());
        } else {
            // «｜King Gnu「SPECIALZ」»: исполнитель прямо перед кавычками
            let start = segs.iter().rev().find(|s| s.start <= sq.start).map(|s| s.start).unwrap_or(0);
            let before = title[start..sq.start].trim();
            if has_letter(before)
                && !marker().is_match(before).unwrap_or(false)
                && !quote().is_match(before).unwrap_or(false)
                && !anime_hint().is_match(before).unwrap_or(false)
            {
                artist = Some(before.to_string());
            }
        }
    } else if let Some(k) = k {
        // Песня — отдельная часть после опенинга: «Chainsaw Man - Opening | KICK BACK»
        let next = segs.iter().skip(k + 1).find(|s| !noise_segment().is_match(&s.text));
        let anime_in_segment = has_letter(&title[segs[k].start..marker_index]);
        if let Some(n) = next {
            song = Some(n.text.clone());
            strong = true;
        } else if k > 0 && anime_in_segment {
            song = Some(segs[k - 1].text.clone()); // «Unravel - Tokyo Ghoul Opening»
        }
        if let Some(s) = song.clone() {
            if let Some(by) = by_artist().captures(&s) {
                song = Some(by["song"].trim().to_string());
                artist = Some(by["artist"].trim().to_string());
            }
        }
    }

    if let Some(aq) = &anime_quote {
        anime = Some(aq.text.clone());
    } else if let Some(k) = k {
        let prefix = quote().replace_all(&title[segs[k].start..marker_index], " ").to_string();
        if has_letter(&prefix) {
            anime = Some(prefix);
        } else if k > 0 && Some(&segs[k - 1].text) != song.as_ref() {
            anime = Some(segs[k - 1].text.clone());
        }
    }
    let anime = anime.map(|a| {
        static TV: OnceLock<Regex> = OnceLock::new();
        let a = r(&TV, r"^\s*TV\s*アニメ|\s{2,}").replace_all(&a, " ").to_string();
        crate::util::trim_chars(&a, &[' ', '-', '–', '—', ':']).to_string()
    });
    Some(Parsed {
        anime: anime.filter(|a| !a.is_empty()),
        kind,
        number: num,
        song: song.filter(|s| !s.trim().is_empty()),
        artist: artist.filter(|a| !a.trim().is_empty()),
        strong,
    })
}

/// Песня из названия, если в нём есть всё нужное и это точно аниме. Без сети.
pub fn from_title(title: &str) -> Option<Song> {
    let p = parse(title)?;
    if p.strong {
        p.song.map(|s| (s, p.artist))
    } else {
        None
    }
}

struct Store {
    /// Ответ по названию ролика; None — не опенинг. Срок — только у ответов, собранных без AnimeThemes.
    cache: HashMap<String, (Option<Song>, Option<Instant>)>,
    pending: HashMap<String, Shared<Pin<Box<dyn Future<Output = Option<Song>> + Send>>>>,
}

fn store() -> &'static Mutex<Store> {
    static S: OnceLock<Mutex<Store>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Store { cache: HashMap::new(), pending: HashMap::new() }))
}

/// Готовый ответ без ожидания; None — его ещё нет.
pub fn try_get_resolved(title: &str) -> Option<Option<Song>> {
    let s = store().lock().ok()?;
    let (song, until) = s.cache.get(title)?;
    if until.map(|u| Instant::now() < u).unwrap_or(true) {
        Some(song.clone())
    } else {
        None
    }
}

/// Песня и исполнитель для ролика с опенингом или эндингом; None — это не он или узнать не удалось.
/// Одновременные запросы одного ролика сливаются в один.
pub async fn resolve(title: String) -> Option<Song> {
    if let Some(song) = try_get_resolved(&title) {
        return song;
    }
    let fut = {
        let Ok(mut s) = store().lock() else { return None };
        match s.pending.get(&title) {
            Some(f) => f.clone(),
            None => {
                let t = title.clone();
                let f: Pin<Box<dyn Future<Output = Option<Song>> + Send>> = Box::pin(look_up(t));
                let shared = f.shared();
                s.pending.insert(title.clone(), shared.clone());
                shared
            }
        }
    };
    let result = fut.await;
    if let Ok(mut s) = store().lock() {
        s.pending.remove(&title);
    }
    result
}

async fn look_up(title: String) -> Option<Song> {
    let parsed = parse(&title);
    let mut result: Option<Song> = None;
    let mut reliable = true;
    if let Some(p) = &parsed {
        if p.strong || p.song.is_some() {
            if p.strong {
                if let Some(s) = &p.song {
                    result = Some((s.clone(), p.artist.clone()));
                }
            }
            if let Some(anime) = &p.anime {
                match search(anime).await {
                    Ok(found) => {
                        if let Some(f) = pick(p, &found) {
                            result = Some(f);
                        }
                    }
                    Err(()) => reliable = false, // AnimeThemes не ответил — обходимся названием, позже спросим снова
                }
            }
        }
    }
    if let Ok(mut s) = store().lock() {
        if s.cache.len() >= CACHE_LIMIT {
            s.cache.clear();
        }
        s.cache.insert(title, (result.clone(), if reliable { None } else { Some(Instant::now() + RETRY_AFTER) }));
    }
    result
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub kind: String,
    pub sequence: Option<i32>,
    pub song: String,
    pub artist: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Anime {
    pub name: String,
    pub synonyms: Vec<String>,
    pub themes: Vec<Theme>,
}

fn key(text: &str) -> String {
    norm(Some(text)).replace(' ', "")
}

/// Слова названия аниме без «Season 2», «2nd», «Part»: у AnimeThemes сезоны записаны по-своему.
fn words(name: &str) -> Vec<String> {
    norm(Some(name)).split(' ').filter(|w| !w.is_empty() && !season_word().is_match(w)).map(String::from).collect()
}

/// Выбор песни среди найденных аниме. Слабое прочтение верим, только если песня совпала.
pub fn pick(parsed: &Parsed, found: &[Anime]) -> Option<Song> {
    if let Some(song) = &parsed.song {
        let wanted = key(song);
        for anime in found {
            for theme in &anime.themes {
                let have = key(&theme.song);
                if have == wanted
                    || (have.chars().count().min(wanted.chars().count()) >= 4 && (have.contains(&wanted) || wanted.contains(&have)))
                {
                    return Some((theme.song.clone(), theme.artist.clone().or_else(|| parsed.artist.clone())));
                }
            }
        }
        return None;
    }
    if !parsed.strong {
        return None;
    }
    let anime_name = parsed.anime.as_ref()?;
    // Песни в названии нет: берём опенинг с нужным номером у аниме, чьё название совпало
    let wanted = words(anime_name);
    for anime in found {
        let names: std::collections::HashSet<String> =
            std::iter::once(&anime.name).chain(anime.synonyms.iter()).flat_map(|n| words(n)).collect();
        if wanted.is_empty() || !wanted.iter().all(|w| names.contains(w)) {
            continue;
        }
        let theme = anime
            .themes
            .iter()
            .find(|t| t.kind == parsed.kind && t.sequence.unwrap_or(1) == parsed.number.unwrap_or(1));
        return theme.map(|t| (t.song.clone(), t.artist.clone()));
    }
    None
}

async fn search(anime: &str) -> Result<Vec<Anime>, ()> {
    let url = format!(
        "https://api.animethemes.moe/search?fields%5Bsearch%5D=anime&page%5Blimit%5D=5&include%5Banime%5D=animesynonyms,animethemes.song.artists&q={}",
        escape(anime)
    );
    let response = slow().get(url).send().await.map_err(|_| ())?;
    if !response.status().is_success() {
        return Err(());
    }
    let doc: serde_json::Value = response.json().await.map_err(|_| ())?;
    let items = doc.get("search").and_then(|s| s.get("anime")).and_then(|a| a.as_array()).ok_or(())?;
    let mut list = Vec::new();
    for item in items {
        let synonyms = item
            .get("animesynonyms")
            .and_then(|s| s.as_array())
            .map(|a| a.iter().map(|s| str_of(s, "text").unwrap_or("").to_string()).collect())
            .unwrap_or_default();
        let mut themes = Vec::new();
        if let Some(all) = item.get("animethemes").and_then(|t| t.as_array()) {
            for t in all {
                let Some(song) = t.get("song").filter(|s| s.is_object()) else { continue };
                let Some(song_title) = str_of(song, "title").filter(|s| !s.is_empty()) else { continue };
                // У песни бывает несколько исполнителей — в базах текстов ищется первый
                let artist = song
                    .get("artists")
                    .and_then(|a| a.as_array())
                    .and_then(|a| a.iter().filter_map(|x| str_of(x, "name")).find(|n| !n.trim().is_empty()))
                    .map(String::from);
                let sequence = t.get("sequence").and_then(|s| s.as_i64()).map(|s| s as i32);
                themes.push(Theme {
                    kind: str_of(t, "type").unwrap_or("").to_string(),
                    sequence,
                    song: song_title.to_string(),
                    artist,
                });
            }
        }
        list.push(Anime { name: str_of(item, "name").unwrap_or("").to_string(), synonyms, themes });
    }
    Ok(list)
}
