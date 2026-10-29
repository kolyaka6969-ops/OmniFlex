//! Тексты песен: сначала свои файлы «Исполнитель - Название.lrc» из папки lyrics, потом открытые базы:
//! синхронные тексты — lrclib.net, NetEase и Kugou одновременно, обычные — text-lyrics.ru, amalgama-lab,
//! Genius и lyrics.ovh. Найденное хранится на диске (lyrics_cache).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use fancy_regex::Regex as FRegex;
use futures::stream::{FuturesUnordered, StreamExt};
use regex::Regex;
use serde::Deserialize;

use super::text::{has_cyrillic, norm, score};
use super::{escape, fallback, lrclib, no_cookies, str_of};
use super::lyrics_cache;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Loading,
    Synced,
    Plain,
    Instrumental,
    NotFound,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub time: Duration,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Lyrics {
    pub status: Status,
    pub lines: Arc<Vec<Line>>,
}

impl Lyrics {
    pub fn of(status: Status) -> Self {
        Self { status, lines: Arc::new(Vec::new()) }
    }
    pub fn loading() -> Self {
        Self::of(Status::Loading)
    }
    pub fn not_found() -> Self {
        Self::of(Status::NotFound)
    }
    pub fn error() -> Self {
        Self::of(Status::Error)
    }
    pub fn instrumental() -> Self {
        Self::of(Status::Instrumental)
    }
    pub fn new(status: Status, lines: Vec<Line>) -> Self {
        Self { status, lines: Arc::new(lines) }
    }

    /// Синхронный текст из LRC; без меток времени — просто текст.
    pub fn from_lrc(lrc: &str, title: Option<&str>, artist: Option<&str>) -> Self {
        let lines = parse_lrc(lrc, title, artist);
        if is_synced(&lines) {
            return Self::new(Status::Synced, lines);
        }
        let text = if lines.is_empty() { lrc.to_string() } else { join_text(&lines) };
        Self::from_plain(&text, title, artist)
    }

    pub fn from_plain(text: &str, title: Option<&str>, artist: Option<&str>) -> Self {
        let mut raw: Vec<String> =
            text.replace('\r', "").split('\n').map(|l| l.trim().to_string()).filter(|l| !is_tag(l)).collect();
        // Весь текст одной строкой: делим там, где после строчной буквы начинается слово с заглавной
        let non_empty: Vec<&String> = raw.iter().filter(|l| !l.is_empty()).collect();
        if non_empty.len() == 1 && non_empty[0].chars().count() > 120 {
            raw = split_sentences(non_empty[0]);
        }
        while !raw.is_empty() && (raw[0].is_empty() || is_header_line(&raw[0], title, artist, None)) {
            raw.remove(0);
        }
        let mut lines: Vec<Line> = raw.into_iter().map(|t| Line { time: Duration::ZERO, text: t }).collect();
        while lines.first().map(|l| l.text.is_empty()).unwrap_or(false) {
            lines.remove(0);
        }
        while lines.last().map(|l| l.text.is_empty()).unwrap_or(false) {
            lines.pop();
        }
        if lines.is_empty() {
            Self::not_found()
        } else {
            Self::new(Status::Plain, lines)
        }
    }
}

fn join_text(lines: &[Line]) -> String {
    lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
}

/// Делит длинную строку там, где после строчной буквы (и, может, знака) идёт пробел и заглавная.
fn split_sentences(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut current = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() && !current.is_empty() {
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let prev: Vec<char> = current.chars().rev().take(2).collect();
            let lower_before = match prev.as_slice() {
                [p, ..] if p.is_lowercase() => true,
                [p, q, ..] if matches!(p, ',' | '!' | '?' | '…') && q.is_lowercase() => true,
                _ => false,
            };
            if lower_before && j < chars.len() && chars[j].is_uppercase() {
                out.push(std::mem::take(&mut current));
                i = j;
                continue;
            }
        }
        current.push(c);
        i += 1;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

pub fn folder() -> std::path::PathBuf {
    crate::util::data_dir().join("lyrics")
}

/// Допуск по длительности: у альбомной версии и сингла она часто расходится на секунду-две.
const DURATION_TOLERANCE: f64 = 3.0;
/// У клипа на YouTube вступление или титры: длительность ролика расходится с треком на секунды.
pub const VIDEO_DURATION_TOLERANCE: f64 = 12.0;
const CACHE_LIMIT: usize = 200;
/// Штраф за транслит: русская песня латиницей хуже любого текста на кириллице.
pub const TRANSLIT_PENALTY: i32 = 4;

fn rx(cell: &'static OnceLock<Regex>, p: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(p).expect("regex"))
}
macro_rules! regex_fn {
    ($name:ident, $p:expr) => {
        fn $name() -> &'static Regex {
            static R: OnceLock<Regex> = OnceLock::new();
            rx(&R, $p)
        }
    };
}

regex_fn!(time_tag, r"\[(\d{1,3}):(\d{1,2})(?:[.:](\d{1,3}))?\]");
regex_fn!(leading_time_tags, r"^(?:\s*\[\d{1,3}:\d{1,2}(?:[.:]\d{1,3})?\])+");
regex_fn!(offset_tag, r"(?i)^\[offset:\s*([+-]?\d+)\s*\]$");
regex_fn!(any_tag, r"(?i)^\[[a-z#]+:.*\]$");
regex_fn!(word_time_tag, r"<\d{1,3}:\d{1,2}(?:[.:]\d{1,3})?>");
regex_fn!(title_suffix, r"(?i)\s+-\s+.*$|\s*[(\[](?:feat|ft|with|prod)\.?\s[^)\]]*[)\]]");
regex_fn!(leading_article, r"(?i)^the\s+");
regex_fn!(artist_separator, r"(?i),\s*|\s+&\s+|\s+(?:feat|ft)\.?\s+");
regex_fn!(unicode_space, r"\p{Zs}");
regex_fn!(zero_width, "[\u{200B}-\u{200D}\u{2060}\u{FEFF}]");
regex_fn!(html_break, r"(?i)<br\s*/?>");
regex_fn!(html_tag, r"<[^>]*>");
regex_fn!(section_header, r"^\[[^\]]*\]$");
regex_fn!(text_lyrics_hit, r#"(?s)<h2 class="title"><a href="(?P<url>https://text-lyrics\.ru/[^"]+\.html)">(?P<name>.*?)</a>"#);
regex_fn!(amalgama_next, r#"(?si)<div class="string_container">\s*<div class="original">\s*<strong>"#);
regex_fn!(amalgama_original, r#"(?si)<div class="original">(.*?)</div>"#);
regex_fn!(non_slug, r"[^a-z0-9]+");
regex_fn!(
    credit_line,
    concat!(
        r"(?i)^\s*(?:作词|作曲|编曲|制作人|制作|混音|母带|和声|吉他|贝斯|鼓|录音|监制|出品|发行|词|曲|",
        r"слова|музыка|текст(?:\s+песни)?|автор(?:ы)?(?:\s+слов)?|композитор|исполнитель|аранжировка|сведение|мастеринг|",
        r"lyrics?(?:\s+by)?|lyricist|written\s+by|composer|composed\s+by|producers?|produced\s+by|arranger|music(?:\s+by)?|",
        r"mix(?:ing|ed)?(?:\s+by)?|master(?:ing|ed)?(?:\s+by)?|vocals?(?:\s+by)?|",
        r"title|track|artist|singer|album|by|lrc(?:\s+by)?|sync(?:\s+by)?|edit(?:\s+by)?|qq(?:\s*群)?|id|hash|sign)\s*[:：$]"
    )
);

fn translit_word() -> &'static FRegex {
    static R: OnceLock<FRegex> = OnceLock::new();
    R.get_or_init(|| FRegex::new(r"(?i)^\p{L}*(?:zh|kh|yu|ya|yo(?!u)|iy|yy)\p{L}*$").expect("regex"))
}

pub fn is_tag(line: &str) -> bool {
    any_tag().is_match(line)
}

fn without_article(s: &str) -> String {
    let stripped = leading_article().replace(s, "").to_string();
    if stripped.trim().is_empty() {
        s.to_string()
    } else {
        stripped
    }
}

fn first_artist(artist: &str) -> String {
    artist_separator().split(artist).next().unwrap_or(artist).trim().to_string()
}

fn clean_spaces(s: &str) -> String {
    zero_width().replace_all(&unicode_space().replace_all(s, " "), "").to_string()
}

const HEADER_DECORATORS: &[char] =
    &['☆', '★', '◆', '◇', '■', '▲', '▼', '♪', '♫', '【', '】', '『', '』', '[', ']', '(', ')', '~', '*', '_', '"', '\'', ' ', '-', '—', '–'];
const TITLE_ARTIST_SEPARATORS: [&str; 6] = [" - ", " — ", " – ", " | ", " : ", " / "];

/// Строка-заголовок ролика/файла («☆Nirvana - Smells Like Teen Spirit☆»), авторы («作词 : ...») или тег,
/// которые некоторые сервисы вставляют первой строкой с меткой 00:00.
pub fn is_header_line(text: &str, title: Option<&str>, artist: Option<&str>, time: Option<Duration>) -> bool {
    if text.trim().is_empty() {
        return true;
    }
    let trimmed = text.trim();
    if credit_line().is_match(trimmed) || is_tag(trimmed) {
        return true;
    }
    let clean = crate::util::trim_chars(trimmed, HEADER_DECORATORS);
    if clean.is_empty() {
        return true;
    }
    let decorated_start = ['☆', '★', '◆', '【', '『'];
    let decorated_end = ['☆', '★', '◆', '】', '』'];
    let has_decorators = trimmed.len() != clean.len()
        && (trimmed.starts_with(&decorated_start[..]) || trimmed.ends_with(&decorated_end[..]));

    // Метаданные авторы файлов помещают только во вступлении (первые 10 секунд)
    if let Some(t) = time {
        if t.as_secs_f64() > 10.0 {
            return false;
        }
    }

    let has_meta = title.map(|t| !t.trim().is_empty()).unwrap_or(false) || artist.map(|a| !a.trim().is_empty()).unwrap_or(false);
    if has_meta {
        let norm_clean = norm(Some(clean));
        let norm_title = norm(title);
        let norm_artist = norm(artist);
        let tl = norm_title.chars().count();
        let al = norm_artist.chars().count();
        if tl >= 2 && norm_clean == norm_title {
            return true;
        }
        if al >= 2 && norm_clean == norm_artist {
            return true;
        }
        for sep in TITLE_ARTIST_SEPARATORS {
            if let Some(i) = clean.find(sep) {
                if i > 0 && i + sep.len() < clean.len() {
                    let left = norm(Some(&clean[..i]));
                    let right = norm(Some(&clean[i + sep.len()..]));
                    let left_title = tl >= 2 && (left == norm_title || left.contains(&norm_title));
                    let right_artist = al >= 2 && (right == norm_artist || right.contains(&norm_artist) || norm_artist.contains(&right));
                    let left_artist = al >= 2 && (left == norm_artist || left.contains(&norm_artist) || norm_artist.contains(&left));
                    let right_title = tl >= 2 && (right == norm_title || right.contains(&norm_title));
                    if (left_title && right_artist) || (left_artist && right_title) {
                        return true;
                    }
                    if (left_title || right_title || left_artist || right_artist)
                        && (has_decorators || time.map(|t| t.is_zero()).unwrap_or(true))
                    {
                        return true;
                    }
                }
            }
        }
    }
    has_decorators && TITLE_ARTIST_SEPARATORS.iter().any(|s| clean.contains(s))
}

/// Убирает вводные заголовки (название, автор, теги) с начала списка строк.
pub fn clean_leading_metadata(mut list: Vec<Line>, title: Option<&str>, artist: Option<&str>) -> Vec<Line> {
    while !list.is_empty() && is_header_line(&list[0].text, title, artist, Some(list[0].time)) {
        list.remove(0);
    }
    list
}

/// Разбирает LRC: «[01:23.45]строка», несколько меток на одной строке, [offset:±мс], пословные метки (убираются).
/// Пустые строки остаются — это паузы в песне.
pub fn parse_lrc(lrc: &str, title: Option<&str>, artist: Option<&str>) -> Vec<Line> {
    let mut result: Vec<Line> = Vec::new();
    let mut offset_ms: i64 = 0;
    for raw in lrc.replace('\r', "").split('\n') {
        let line = raw.trim();
        if let Some(o) = offset_tag().captures(line) {
            // Положительный offset — текст должен появляться раньше
            offset_ms = o[1].parse().unwrap_or(0);
            continue;
        }
        let Some(tags) = leading_time_tags().find(line) else { continue };
        let text = word_time_tag().replace_all(&line[tags.end()..], "").trim().to_string();
        let text = super::text::multi_space().replace_all(&text, " ").to_string();
        for m in time_tag().captures_iter(tags.as_str()) {
            let minutes: f64 = m[1].parse().unwrap_or(0.0);
            let seconds: f64 = m[2].parse().unwrap_or(0.0);
            let fraction = m.get(3).map(|f| f.as_str().parse::<f64>().unwrap_or(0.0) / 10f64.powi(f.as_str().len() as i32)).unwrap_or(0.0);
            let time = Duration::from_secs_f64(minutes * 60.0 + seconds + fraction);
            result.push(Line { time, text: text.clone() });
        }
    }
    if offset_ms != 0 {
        for l in &mut result {
            let t = l.time.as_millis() as i64 - offset_ms;
            l.time = Duration::from_millis(t.max(0) as u64);
        }
    }
    // Устойчивая сортировка: у повторяющихся припевов метки часто идут не по порядку
    result.sort_by_key(|l| l.time);
    let sorted = clean_leading_metadata(result, title, artist);
    // Пустые строки в начале и подряд ничего не дают
    let mut clean: Vec<Line> = Vec::with_capacity(sorted.len());
    for l in sorted {
        if !l.text.is_empty() || clean.last().map(|c| !c.text.is_empty()).unwrap_or(false) {
            clean.push(l);
        }
    }
    clean
}

/// На lrclib попадается «синхронный» текст, где всё одной строкой с меткой 00:00 — это не синхронизация.
pub fn is_synced(lines: &[Line]) -> bool {
    let mut times: Vec<Duration> = lines.iter().map(|l| l.time).collect();
    times.dedup();
    let mut distinct = times;
    distinct.sort();
    distinct.dedup();
    distinct.len() >= 3
}

/// Битые метки: куплет целиком уложен в пару секунд. Три быстрые смены строк подряд в настоящей песне не бывают.
pub fn is_broken_sync(lines: &[Line]) -> bool {
    let mut fast = 0;
    let mut previous: Option<&Line> = None;
    for line in lines {
        if line.text.is_empty() {
            continue; // пустые строки — паузы, они бывают короткими
        }
        if let Some(p) = previous {
            if line.time.saturating_sub(p.time).as_secs_f64() < 0.75 && line.time >= p.time {
                fast += 1;
                if fast >= 3 {
                    return true;
                }
            } else {
                fast = 0;
            }
        }
        previous = Some(line);
    }
    false
}

/// Русский текст латиницей («Ya prishol daty etu piesnyu»).
pub fn is_transliteration<'a>(lines: impl Iterator<Item = &'a str>) -> bool {
    const SEPARATORS: &[char] = &[' ', ',', '.', '!', '?', ';', ':', '"', '«', '»', '(', ')', '-', '—', '…', '\''];
    let (mut words, mut marked) = (0, 0);
    for line in lines {
        for word in line.split(SEPARATORS).filter(|w| !w.is_empty()) {
            if word.chars().any(|c| ('Ѐ'..='ӿ').contains(&c)) {
                return false;
            }
            words += 1;
            if translit_word().is_match(word).unwrap_or(false) {
                marked += 1;
            }
        }
    }
    marked >= 3 && marked as f64 >= words as f64 * 0.15
}

/// Насколько текст плох, меньше — лучше: синхронный 0, обычный 2, плюс штраф за транслит.
pub fn rank(lyrics: &Lyrics) -> i32 {
    let r = match lyrics.status {
        Status::Synced => 0,
        Status::Plain | Status::Instrumental => 2,
        _ => 10,
    };
    if !lyrics.lines.is_empty() && is_transliteration(lyrics.lines.iter().map(|l| l.text.as_str())) {
        r + TRANSLIT_PENALTY
    } else {
        r
    }
}

// ---------- lrclib ----------

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct LrcLibTrack {
    pub duration: f64,
    pub instrumental: bool,
    pub plain_lyrics: Option<String>,
    pub synced_lyrics: Option<String>,
    pub track_name: Option<String>,
    pub artist_name: Option<String>,
}

fn to_lyrics(t: &LrcLibTrack) -> Option<Lyrics> {
    let synced = match &t.synced_lyrics {
        Some(s) if !s.trim().is_empty() => parse_lrc(s, t.track_name.as_deref(), t.artist_name.as_deref()),
        _ => Vec::new(),
    };
    // Битые метки врали бы — такой текст показываем без них
    if is_synced(&synced) && !is_broken_sync(&synced) {
        return Some(Lyrics::new(Status::Synced, synced));
    }
    if let Some(p) = t.plain_lyrics.as_deref().filter(|p| !p.trim().is_empty()) {
        return Some(Lyrics::from_plain(p, t.track_name.as_deref(), t.artist_name.as_deref()));
    }
    if !synced.is_empty() {
        return Some(Lyrics::from_plain(&join_text(&synced), t.track_name.as_deref(), t.artist_name.as_deref()));
    }
    if t.instrumental {
        Some(Lyrics::instrumental())
    } else {
        None
    }
}

fn is_candidate_match(t: &LrcLibTrack, title: Option<&str>, artist: Option<&str>) -> bool {
    let Some(artist) = artist.filter(|a| !a.trim().is_empty()) else { return true };
    let Some(track_artist) = t.artist_name.as_deref().filter(|a| !a.trim().is_empty()) else { return true };
    let wanted = title.or(t.track_name.as_deref()).unwrap_or("");
    let found = t.track_name.as_deref().or(title).unwrap_or("");
    score(wanted, Some(artist), Some(found), Some(track_artist)) >= 4
}

/// Лучший из найденных с подходящей длительностью: меньший rank, потом ближе по длительности.
fn pick_track(found: &[LrcLibTrack], seconds: f64, tolerance: f64, title: Option<&str>, artist: Option<&str>) -> Option<(Lyrics, i32)> {
    let mut best: Option<(Lyrics, i32, f64)> = None;
    for t in found {
        if !(seconds <= 0.0 || (t.duration - seconds).abs() <= tolerance) || !is_candidate_match(t, title, artist) {
            continue;
        }
        let Some(l) = to_lyrics(t) else { continue };
        let r = rank(&l);
        let d = (t.duration - seconds).abs();
        if best.as_ref().map(|(_, br, bd)| r < *br || (r == *br && d < *bd)).unwrap_or(true) {
            best = Some((l, r, d));
        }
    }
    best.map(|(l, r, _)| (l, r))
}

fn query(items: &[(&str, &str)]) -> String {
    items.iter().map(|(n, v)| format!("{n}={}", escape(v))).collect::<Vec<_>>().join("&")
}

/// Запрос к lrclib; None — не ответил. Под нагрузкой он отвечает 503 или 429 — тогда одна быстрая повторная попытка.
async fn get_lrclib<T: serde::de::DeserializeOwned>(path_and_query: &str) -> Option<T> {
    for attempt in 0..2 {
        let response = lrclib().get(format!("https://lrclib.net/api/{path_and_query}")).send().await.ok()?;
        let status = response.status();
        if status.is_success() {
            return response.json::<T>().await.ok();
        }
        if attempt > 0 || !(status.as_u16() == 503 || status.as_u16() == 429) {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    None
}

async fn search(title: &str, artist: Option<&str>) -> Option<Vec<LrcLibTrack>> {
    let q = match artist {
        Some(a) => query(&[("track_name", title), ("artist_name", a)]),
        None => query(&[("track_name", title)]),
    };
    get_lrclib::<Vec<LrcLibTrack>>(&format!("search?{q}")).await
}

/// Название с артиклем ищется ещё и без него: на lrclib у «The 7th Element» только транслит.
async fn search_candidate(title: String, artist: Option<String>) -> Option<Vec<LrcLibTrack>> {
    let bare = without_article(&title);
    if bare == title || bare.chars().count() < 2 {
        return search(&title, artist.as_deref()).await;
    }
    let (a, b) = tokio::join!(search(&title, artist.as_deref()), search(&bare, artist.as_deref()));
    if a.is_none() && b.is_none() {
        return None;
    }
    let mut all = a.unwrap_or_default();
    all.extend(b.unwrap_or_default());
    Some(all)
}

/// Точное совпадение. Если трека нет в базе, lrclib ищет его у других сервисов и отвечает 503 — это не ошибка.
async fn get_exact(q: String) -> Option<Vec<LrcLibTrack>> {
    let work = async {
        let r = lrclib().get(format!("https://lrclib.net/api/get?{q}")).send().await.ok()?;
        if !r.status().is_success() {
            return Some(Vec::new());
        }
        Some(r.json::<LrcLibTrack>().await.map(|t| vec![t]).unwrap_or_default())
    };
    Some(tokio::time::timeout(Duration::from_millis(1800), work).await.ok().flatten().unwrap_or_default())
}

/// Варианты названия и исполнителя для поиска, без повторов.
fn variants(title: &str, artist: Option<&str>) -> Vec<(String, Option<String>)> {
    let mut list: Vec<(String, Option<String>)> = Vec::new();
    let mut add = |t: &str, a: Option<String>| {
        let t = t.trim().to_string();
        if t.chars().count() >= 2
            && !list.iter().any(|(lt, la)| lt.to_lowercase() == t.to_lowercase() && la.as_ref().map(|x| x.to_lowercase()) == a.as_ref().map(|x| x.to_lowercase()))
        {
            list.push((t, a));
        }
    };
    add(title, artist.map(String::from));
    let mut short = title_suffix().replace_all(title, "").trim().to_string();
    if short.is_empty() {
        short = title.to_string();
    }
    let first = artist.map(first_artist);
    add(&short, first.clone());
    add(&without_article(&short), first);
    list
}

/// Точный запрос get и поиск по вариантам названия идут одновременно. Первый хороший синхронный текст
/// отменяет остальные запросы.
async fn fetch_lrclib(title: &str, artist: Option<&str>, album: Option<&str>, duration: Duration) -> Lyrics {
    let seconds = duration.as_secs_f64().round();
    let mut pending = FuturesUnordered::new();
    if let (Some(a), true) = (artist, seconds > 0.0) {
        let q = query(&[
            ("track_name", title),
            ("artist_name", a),
            ("album_name", album.unwrap_or("")),
            ("duration", &format!("{seconds}")),
        ]);
        pending.push(Box::pin(get_exact(q)) as std::pin::Pin<Box<dyn std::future::Future<Output = Option<Vec<LrcLibTrack>>> + Send>>);
    }
    for (t, a) in variants(title, artist) {
        pending.push(Box::pin(async move { search(&t, a.as_deref()).await }));
    }
    let mut best: Option<Lyrics> = None;
    let mut best_rank = i32::MAX;
    let mut failed = false;
    while let Some(found) = pending.next().await {
        let Some(found) = found else {
            failed = true;
            continue;
        };
        if let Some((l, r)) = pick_track(&found, seconds, DURATION_TOLERANCE, Some(title), artist) {
            if r < best_rank {
                best = Some(l);
                best_rank = r;
            }
        }
        if best_rank == 0 {
            break;
        }
    }
    best.unwrap_or_else(|| if failed { Lyrics::error() } else { Lyrics::not_found() })
}

// ---------- Другие базы ----------

async fn with_timeout<F: std::future::Future<Output = Lyrics>>(secs: f32, f: F) -> Lyrics {
    tokio::time::timeout(Duration::from_secs_f32(secs), f).await.unwrap_or_else(|_| Lyrics::error())
}

async fn get_text(client: &reqwest::Client, url: &str) -> Result<String, ()> {
    let r = client.get(url).send().await.map_err(|_| ())?;
    if !r.status().is_success() {
        return Err(());
    }
    r.text().await.map_err(|_| ())
}

/// lyrics.ovh — резервная база без ключа: обычный текст, когда lrclib недоступен.
async fn fetch_ovh(title: String, artist: Option<String>) -> Lyrics {
    let Some(artist) = artist.filter(|a| !a.trim().is_empty()) else { return Lyrics::error() };
    let url = format!("https://api.lyrics.ovh/v1/{}/{}", escape(&first_artist(&artist)), escape(&title));
    let Ok(r) = fallback().get(url).send().await else { return Lyrics::error() };
    if r.status().as_u16() == 404 {
        return Lyrics::not_found();
    }
    if !r.status().is_success() {
        return Lyrics::error();
    }
    let Ok(doc) = r.json::<serde_json::Value>().await else { return Lyrics::error() };
    let Some(raw) = str_of(&doc, "lyrics") else { return Lyrics::not_found() };
    // Французская подпись сервиса: «Paroles de la chanson … par …»
    let text: Vec<&str> = raw
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim_start().to_lowercase().starts_with("paroles de la chanson"))
        .collect();
    let l = Lyrics::from_plain(&text.join("\n"), None, None);
    if l.status == Status::NotFound {
        Lyrics::not_found()
    } else {
        l
    }
}

/// Genius: текстов там больше всего, но без меток времени. Берём только песню с тем же названием и исполнителем.
async fn fetch_genius(title: String, artist: Option<String>) -> Lyrics {
    with_timeout(3.5, async move {
        let q = escape(&match artist.as_deref().filter(|a| !a.trim().is_empty()) {
            Some(a) => format!("{} {title}", first_artist(a)),
            None => title.clone(),
        });
        let Ok(json) = get_text(fallback(), &format!("https://genius.com/api/search/song?q={q}&per_page=5")).await else {
            return Lyrics::error();
        };
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(&json) else { return Lyrics::error() };
        let Some(hits) = doc.pointer("/response/sections/0/hits").and_then(|h| h.as_array()) else {
            return Lyrics::not_found();
        };
        let mut best = 0;
        let mut song: Option<&serde_json::Value> = None;
        for hit in hits {
            let Some(result) = hit.get("result") else { continue };
            let found_artist = str_of(result, "artist_names").or(result.get("primary_artist").and_then(|p| str_of(p, "name")));
            let s = score(&title, artist.as_deref(), str_of(result, "title"), found_artist);
            if s > best {
                best = s;
                song = Some(result);
            }
        }
        let threshold = if artist.as_deref().map(|a| a.trim().is_empty()).unwrap_or(true) { 3 } else { 4 };
        let Some(s) = song.filter(|_| best >= threshold) else { return Lyrics::not_found() };
        if s.get("instrumental").and_then(|i| i.as_bool()) == Some(true) {
            return Lyrics::instrumental();
        }
        let Some(url) = str_of(s, "url").filter(|u| u.starts_with("https://genius.com/")) else { return Lyrics::not_found() };
        let Ok(page) = get_text(fallback(), url).await else { return Lyrics::error() };
        match parse_genius_page(&page) {
            Some(text) => Lyrics::from_plain(&text, None, None),
            None => Lyrics::not_found(),
        }
    })
    .await
}

/// Индекс закрывающего тега элемента, содержимое которого начинается со start.
fn element_end(html: &str, start: usize, tag: &str) -> usize {
    let lower = html.to_lowercase();
    // Длины совпадают только для ASCII; для разметки этого достаточно, но защищаемся от расхождения
    let hay = if lower.len() == html.len() { lower } else { html.to_string() };
    let open_tag = format!("<{tag}");
    let close_tag = format!("</{tag}");
    let mut depth = 1;
    let mut i = start;
    loop {
        let open = hay[i..].find(&open_tag).map(|p| p + i);
        let Some(close) = hay[i..].find(&close_tag).map(|p| p + i) else { return html.len() };
        if let Some(o) = open.filter(|o| *o < close) {
            depth += 1;
            i = o + tag.len() + 1;
        } else {
            depth -= 1;
            if depth == 0 {
                return close;
            }
            i = close + tag.len() + 2;
        }
    }
}

/// Вырезает элементы с data-exclude-from-selection: «107 Contributors», переводы, «Read More».
fn remove_excluded(html: &str) -> String {
    const MARKER: &str = "data-exclude-from-selection=\"true\"";
    let mut html = html.to_string();
    while let Some(at) = html.find(MARKER) {
        let Some(tag_start) = html[..at].rfind('<') else { break };
        let Some(name_end) = html[tag_start + 1..].find([' ', '>']).map(|p| p + tag_start + 1) else { break };
        let Some(content_start) = html[at..].find('>').map(|p| p + at + 1) else { break };
        let tag = html[tag_start + 1..name_end].to_string();
        let close = element_end(&html, content_start, &tag);
        let after = html[close..].find('>').map(|p| p + close);
        html = format!("{}{}", &html[..tag_start], after.map(|a| &html[a + 1..]).unwrap_or(""));
    }
    html
}

/// Текст со страницы Genius: блоки data-lyrics-container без служебных вставок.
pub fn parse_genius_page(html: &str) -> Option<String> {
    let mut parts = Vec::new();
    let mut at = 0;
    while let Some(p) = html[at..].find("data-lyrics-container=\"true\"") {
        let pos = at + p;
        let Some(start) = html[pos..].find('>').map(|x| x + pos + 1) else { break };
        let end = element_end(html, start, "div");
        parts.push(remove_excluded(&html[start..end]));
        at = end.max(start);
        if at >= html.len() {
            break;
        }
    }
    if parts.is_empty() {
        None
    } else {
        html_to_lyrics(&parts.join("<br>"))
    }
}

/// HTML с текстом в строки: <br> — перевод строки, теги и сущности убраны, заголовки частей
/// «[Chorus: …]» — пустая строка между куплетами.
fn html_to_lyrics(html: &str) -> Option<String> {
    let flat = html.replace('\r', "").replace('\n', " ");
    let text = html_tag().replace_all(&html_break().replace_all(&flat, "\n"), "").to_string();
    let text = html_escape::decode_html_entities(&text).to_string();
    let text = clean_spaces(&text);
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let mut line = super::text::multi_space().replace_all(raw, " ").trim().to_string();
        if section_header().is_match(&line) {
            line.clear();
        }
        // Между куплетами одна пустая строка
        if !line.is_empty() || lines.last().map(|l| !l.is_empty()).unwrap_or(false) {
            lines.push(line);
        }
    }
    let result = lines.join("\n").trim().to_string();
    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// text-lyrics.ru — русская база текстов: там есть песни, которых нет ни на lrclib, ни на Genius, ни на NetEase.
async fn fetch_text_lyrics(title: String, artist: Option<String>) -> Lyrics {
    let Some(artist) = artist.filter(|a| !a.trim().is_empty()) else { return Lyrics::not_found() };
    with_timeout(3.5, async move {
        let q = escape(&format!("{} {title}", first_artist(&artist)));
        let Ok(page) = get_text(fallback(), &format!("https://text-lyrics.ru/index.php?do=search&subaction=search&story={q}")).await
        else {
            return Lyrics::error();
        };
        let mut best = 0;
        let mut url: Option<String> = None;
        for m in text_lyrics_hit().captures_iter(&page) {
            let name = html_escape::decode_html_entities(&html_tag().replace_all(&m["name"], "")).trim().to_string();
            let dash = name.find(" - ").or_else(|| name.find(" – ")).or_else(|| name.find(" — "));
            let Some(dash) = dash.filter(|d| *d > 0) else { continue };
            let sep_len = name[dash..].chars().take(3).map(|c| c.len_utf8()).sum::<usize>();
            let s = score(&title, Some(&artist), Some(&name[dash + sep_len..]), Some(&name[..dash]));
            if s > best {
                best = s;
                url = Some(m["url"].to_string());
            }
        }
        let Some(url) = url.filter(|_| best >= 4) else { return Lyrics::not_found() };
        let Ok(page) = get_text(fallback(), &url).await else { return Lyrics::error() };
        match parse_text_lyrics_page(&page) {
            Some(t) => Lyrics::from_plain(&t, None, None),
            None => Lyrics::not_found(),
        }
    })
    .await
}

pub fn parse_text_lyrics_page(html: &str) -> Option<String> {
    let at = html.find("id=\"entry_content\"")?;
    let start = html[at..].find('>')? + at + 1;
    html_to_lyrics(&html[start..element_end(html, start, "div")])
}

/// NetEase Cloud Music: огромный каталог и часто синхронный текст. Метки — только если длительность сходится.
async fn fetch_netease(title: String, artist: Option<String>, seconds: f64, tolerance: f64) -> Lyrics {
    with_timeout(3.5, async move {
        let q = escape(&match artist.as_deref().filter(|a| !a.trim().is_empty()) {
            Some(a) => format!("{} {title}", first_artist(a)),
            None => title.clone(),
        });
        let Ok(json) = get_text(no_cookies(), &format!("https://music.163.com/api/search/get?s={q}&type=1&limit=8")).await else {
            return Lyrics::error();
        };
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(&json) else { return Lyrics::error() };
        let Some(songs) = doc.pointer("/result/songs").and_then(|s| s.as_array()) else { return Lyrics::not_found() };
        let threshold = if artist.as_deref().map(|a| a.trim().is_empty()).unwrap_or(true) { 3 } else { 4 };
        let mut matches: Vec<(i64, f64, i32)> = songs
            .iter()
            .map(|song| {
                let artists = song
                    .get("artists")
                    .and_then(|l| l.as_array())
                    .map(|l| l.iter().filter_map(|a| str_of(a, "name")).collect::<Vec<_>>().join(", "));
                let length = song.get("duration").and_then(|d| d.as_f64()).map(|ms| ms / 1000.0).unwrap_or(0.0);
                let id = song.get("id").and_then(|i| i.as_i64()).unwrap_or(0);
                (id, length, score(&title, artist.as_deref(), str_of(song, "name"), artists.as_deref()))
            })
            .filter(|m| m.0 != 0 && m.2 >= threshold)
            .collect();
        matches.sort_by(|a, b| {
            b.2.cmp(&a.2).then_with(|| {
                let da = if seconds > 0.0 && a.1 > 0.0 { (a.1 - seconds).abs() } else { 0.0 };
                let db = if seconds > 0.0 && b.1 > 0.0 { (b.1 - seconds).abs() } else { 0.0 };
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        matches.truncate(3);
        // У части версий (live, ремастер) текста на NetEase нет — берём первую, где он есть
        let mut lrc: Option<String> = None;
        let mut duration = 0.0;
        for (id, length, _) in &matches {
            let Ok(json) = get_text(no_cookies(), &format!("https://music.163.com/api/song/lyric?id={id}&lv=1")).await else {
                return Lyrics::error();
            };
            let Ok(root) = serde_json::from_str::<serde_json::Value>(&json) else { return Lyrics::error() };
            if root.get("pureMusic").and_then(|p| p.as_bool()) == Some(true) {
                return Lyrics::instrumental();
            }
            lrc = root.get("lrc").and_then(|l| str_of(l, "lyric")).map(String::from);
            duration = *length;
            if lrc.as_deref().map(|l| !l.trim().is_empty()).unwrap_or(false) {
                break;
            }
        }
        let Some(lrc) = lrc.filter(|l| !l.trim().is_empty()) else { return Lyrics::not_found() };
        timed_or_plain(&clean_spaces(&lrc), &title, artist.as_deref(), seconds, duration, tolerance, false)
    })
    .await
}

/// LRC после NetEase и Kugou: без строк авторов; метки — только если длительность сходится.
fn timed_or_plain(lrc: &str, title: &str, artist: Option<&str>, seconds: f64, duration: f64, tolerance: f64, check_broken: bool) -> Lyrics {
    let mut lines: Vec<Line> =
        parse_lrc(lrc, Some(title), artist).into_iter().filter(|l| !credit_line().is_match(&l.text)).collect();
    while lines.first().map(|l| l.text.is_empty()).unwrap_or(false) {
        lines.remove(0);
    }
    let timed = seconds <= 0.0 || duration <= 0.0 || (duration - seconds).abs() <= tolerance;
    if timed && is_synced(&lines) && !(check_broken && is_broken_sync(&lines)) {
        return Lyrics::new(Status::Synced, lines);
    }
    if lines.is_empty() {
        Lyrics::not_found()
    } else {
        Lyrics::from_plain(&join_text(&lines), None, None)
    }
}

/// Kugou Music — огромная база синхронных текстов без ключей и регистрации.
async fn fetch_kugou(title: String, artist: Option<String>, seconds: f64, tolerance: f64) -> Lyrics {
    with_timeout(3.5, async move {
        let q = escape(&match artist.as_deref().filter(|a| !a.trim().is_empty()) {
            Some(a) => format!("{} {title}", first_artist(a)),
            None => title.clone(),
        });
        let url = format!("http://mobilecdn.kugou.com/api/v3/search/song?format=json&keyword={q}&page=1&pagesize=6");
        let Ok(json) = get_text(no_cookies(), &url).await else { return Lyrics::error() };
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(&json) else { return Lyrics::error() };
        let Some(info) = doc.pointer("/data/info").and_then(|i| i.as_array()) else { return Lyrics::not_found() };
        let threshold = if artist.as_deref().map(|a| a.trim().is_empty()).unwrap_or(true) { 3 } else { 4 };
        let mut matches: Vec<(String, f64, i32)> = info
            .iter()
            .map(|song| {
                let s = score(&title, artist.as_deref(), str_of(song, "songname"), str_of(song, "singername"));
                (str_of(song, "hash").unwrap_or("").to_string(), song.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0), s)
            })
            .filter(|m| !m.0.is_empty() && m.2 >= threshold)
            .collect();
        matches.sort_by(|a, b| {
            b.2.cmp(&a.2).then_with(|| {
                let da = if seconds > 0.0 && a.1 > 0.0 { (a.1 - seconds).abs() } else { 0.0 };
                let db = if seconds > 0.0 && b.1 > 0.0 { (b.1 - seconds).abs() } else { 0.0 };
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
        });
        matches.truncate(2);
        if matches.is_empty() {
            return Lyrics::not_found();
        }
        let mut plain_fallback: Option<Lyrics> = None;
        for (hash, duration, _) in &matches {
            let url = format!("http://lyrics.kugou.com/search?ver=1&man=yes&client=pc&keyword={q}&hash={hash}");
            let Ok(json) = get_text(no_cookies(), &url).await else { return Lyrics::error() };
            let Ok(cand_doc) = serde_json::from_str::<serde_json::Value>(&json) else { continue };
            let Some(cand) = cand_doc.get("candidates").and_then(|c| c.as_array()).and_then(|c| c.first()) else { continue };
            let (Some(id), Some(key)) = (str_of(cand, "id"), str_of(cand, "accesskey")) else { continue };
            let url = format!("http://lyrics.kugou.com/download?ver=1&client=pc&id={id}&accesskey={key}&fmt=lrc&charset=utf8");
            let Ok(json) = get_text(no_cookies(), &url).await else { return Lyrics::error() };
            let Ok(dl) = serde_json::from_str::<serde_json::Value>(&json) else { continue };
            let Some(content) = str_of(&dl, "content").filter(|c| !c.trim().is_empty()) else { continue };
            use base64::Engine;
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(content) else { continue };
            let lrc = String::from_utf8_lossy(&bytes).trim_matches('\u{FEFF}').to_string();
            if lrc.trim().is_empty() {
                continue;
            }
            let l = timed_or_plain(&clean_spaces(&lrc), &title, artist.as_deref(), seconds, *duration, tolerance, true);
            if l.status == Status::Synced {
                return l;
            }
            if l.status == Status::Plain && plain_fallback.is_none() {
                plain_fallback = Some(l);
            }
        }
        plain_fallback.unwrap_or_else(Lyrics::not_found)
    })
    .await
}

fn slug(text: &str) -> String {
    let s = text.trim().to_lowercase();
    let s = without_article(&s);
    let s = title_suffix().replace_all(&s, "");
    non_slug().replace_all(&s, "_").trim_matches('_').to_string()
}

/// amalgama-lab.com — крупнейшая в СНГ база оригинальных текстов зарубежных песен с переводами.
async fn fetch_amalgama(title: String, artist: Option<String>) -> Lyrics {
    let Some(artist) = artist.filter(|a| !a.trim().is_empty()) else { return Lyrics::not_found() };
    with_timeout(3.5, async move {
        let artist_slug = slug(&first_artist(&artist));
        let title_slug = slug(&title);
        if artist_slug.is_empty() || title_slug.is_empty() {
            return Lyrics::not_found();
        }
        let letter = artist_slug.chars().next().unwrap();
        let url = format!("https://www.amalgama-lab.com/songs/{letter}/{artist_slug}/{title_slug}.html");
        let Ok(r) = fallback().get(url).send().await else { return Lyrics::error() };
        if r.status().as_u16() == 404 {
            return Lyrics::not_found();
        }
        if !r.status().is_success() {
            return Lyrics::error();
        }
        let Ok(bytes) = r.bytes().await else { return Lyrics::error() };
        let (html, _, _) = encoding_rs::WINDOWS_1251.decode(&bytes);
        match parse_amalgama_page(&html) {
            Some(t) => Lyrics::from_plain(&t, None, None),
            None => Lyrics::not_found(),
        }
    })
    .await
}

pub fn parse_amalgama_page(html: &str) -> Option<String> {
    let at = html.find("id=\"click_area\"").or_else(|| html.find("class=\"texts col\""))?;
    let mut sub = &html[at..];
    if let Some(m) = amalgama_next().find(sub) {
        sub = &sub[..m.start()];
    }
    let lines: Vec<String> = amalgama_original().captures_iter(sub).map(|c| c[1].to_string()).collect();
    if lines.is_empty() {
        None
    } else {
        html_to_lyrics(&lines.join("<br>"))
    }
}

type LFut = std::pin::Pin<Box<dyn std::future::Future<Output = (usize, Lyrics)> + Send>>;

fn tagged(id: usize, f: impl std::future::Future<Output = Lyrics> + Send + 'static) -> LFut {
    Box::pin(async move { (id, f.await) })
}

/// Первая очередь: синхронные тексты одновременно с lrclib, NetEase и Kugou. Хороший синхронный текст
/// (не транслит) сразу отменяет остальные запросы.
async fn fetch_timed(title: &str, artist: Option<&str>, album: Option<&str>, duration: Duration) -> (Option<Lyrics>, Option<Lyrics>) {
    let seconds = duration.as_secs_f64().round();
    let (t, a, al) = (title.to_string(), artist.map(String::from), album.map(String::from));
    let mut pending: FuturesUnordered<LFut> = FuturesUnordered::new();
    {
        let (t, a) = (t.clone(), a.clone());
        pending.push(tagged(0, async move { fetch_lrclib(&t, a.as_deref(), al.as_deref(), duration).await }));
    }
    pending.push(tagged(1, fetch_netease(t.clone(), a.clone(), seconds, DURATION_TOLERANCE)));
    pending.push(tagged(2, fetch_kugou(t, a, seconds, DURATION_TOLERANCE)));
    let mut best_synced: Option<Lyrics> = None;
    let mut best_rank = i32::MAX;
    let mut best_plain: Option<Lyrics> = None;
    while let Some((_, result)) = pending.next().await {
        match result.status {
            Status::Synced => {
                let r = rank(&result);
                if r < TRANSLIT_PENALTY {
                    return (Some(result), None);
                }
                if r < best_rank {
                    best_rank = r;
                    best_synced = Some(result);
                }
            }
            Status::Plain | Status::Instrumental => {
                if best_plain.is_none() {
                    best_plain = Some(result);
                }
            }
            _ => {}
        }
    }
    (best_synced, best_plain)
}

/// Вторая очередь: обычный текст. Для русских песен — text-lyrics.ru и Genius, для остальных — amalgama-lab,
/// lyrics.ovh и Genius, плюс запасной текст из первой очереди.
async fn fetch_plain_tier(title: &str, artist: Option<&str>, fallback_plain: Option<Lyrics>) -> Lyrics {
    let russian = has_cyrillic(Some(title)) || has_cyrillic(artist);
    let (t, a) = (title.to_string(), artist.map(String::from));
    let mut pending: FuturesUnordered<LFut> = FuturesUnordered::new();
    const TEXT_LYRICS: usize = 0;
    const AMALGAMA: usize = 1;
    if russian {
        pending.push(tagged(TEXT_LYRICS, fetch_text_lyrics(t.clone(), a.clone())));
    } else {
        pending.push(tagged(AMALGAMA, fetch_amalgama(t.clone(), a.clone())));
        pending.push(tagged(3, fetch_ovh(t.clone(), a.clone())));
    }
    pending.push(tagged(2, fetch_genius(t, a)));
    let total = pending.len();

    let mut best = fallback_plain.filter(|l| !matches!(l.status, Status::NotFound | Status::Error));
    let mut instrumental = best.as_ref().map(|b| b.status == Status::Instrumental).unwrap_or(false);
    let mut errors = 0;
    let mut preferred_done = false;
    while let Some((id, result)) = pending.next().await {
        let preferred = (russian && id == TEXT_LYRICS) || (!russian && id == AMALGAMA);
        if preferred {
            preferred_done = true;
        }
        match result.status {
            Status::Instrumental => instrumental = true,
            Status::Error => errors += 1,
            Status::Plain => {
                // Аутентичный текст с text-lyrics.ru (русские песни) или amalgama-lab (зарубежные) возвращаем сразу
                if preferred {
                    return result;
                }
                if best.is_none() {
                    best = Some(result);
                }
            }
            _ => {}
        }
        if let Some(b) = &best {
            if b.status == Status::Plain && preferred_done {
                return b.clone();
            }
        }
    }
    if let Some(b) = best {
        return b;
    }
    if instrumental {
        return Lyrics::instrumental();
    }
    if errors == total {
        Lyrics::error()
    } else {
        Lyrics::not_found()
    }
}

/// Genius, NetEase и text-lyrics.ru одновременно: синхронный текст NetEase лучше всего, потом обычный.
async fn fetch_second_tier(title: &str, artist: Option<&str>, seconds: f64, tolerance: f64) -> Lyrics {
    let russian = has_cyrillic(Some(title)) || has_cyrillic(artist);
    let (t, a) = (title.to_string(), artist.map(String::from));
    const GENIUS: usize = 0;
    const NETEASE: usize = 1;
    const TEXT_LYRICS: usize = 2;
    let mut pending: FuturesUnordered<LFut> = FuturesUnordered::new();
    pending.push(tagged(GENIUS, fetch_genius(t.clone(), a.clone())));
    pending.push(tagged(NETEASE, fetch_netease(t.clone(), a.clone(), seconds, tolerance)));
    pending.push(tagged(TEXT_LYRICS, fetch_text_lyrics(t, a)));
    let mut best_plain: Option<Lyrics> = None;
    let mut errors = 0;
    let mut instrumental = false;
    let mut netease_done = false;
    while let Some((id, result)) = pending.next().await {
        if id == NETEASE {
            netease_done = true;
        }
        match result.status {
            Status::Synced => return result,
            Status::Instrumental => instrumental = true,
            Status::Error => errors += 1,
            Status::Plain => {
                if russian && id == TEXT_LYRICS {
                    return result;
                }
                if best_plain.is_none() || (!russian && id == GENIUS) {
                    best_plain = Some(result);
                }
            }
            _ => {}
        }
        if best_plain.is_some() && (seconds <= 0.0 || netease_done) {
            return best_plain.unwrap();
        }
    }
    if let Some(b) = best_plain {
        return b;
    }
    if instrumental {
        return Lyrics::instrumental();
    }
    if errors == 3 {
        Lyrics::error()
    } else {
        Lyrics::not_found()
    }
}

// ---------- Свои файлы и кэш ----------

pub fn safe_file_name(name: &str) -> String {
    let invalid = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    let s: String = name.chars().map(|c| if invalid.contains(&c) || (c as u32) < 32 { '_' } else { c }).collect();
    s.trim().trim_end_matches('.').to_string()
}

/// Свой файл можно положить для песен, которых нет в базах, или чтобы поправить чужой.
pub fn find_local(title: &str, artist: Option<&str>) -> Option<Lyrics> {
    let dir = folder();
    if !dir.exists() {
        return None;
    }
    let mut names = match artist {
        Some(a) => vec![format!("{a} - {title}"), format!("{} - {title}", first_artist(a)), title.to_string()],
        None => vec![title.to_string()],
    };
    names.dedup();
    for name in names {
        let base = dir.join(safe_file_name(&name));
        if let Ok(text) = std::fs::read_to_string(base.with_extension("lrc")) {
            return Some(Lyrics::from_lrc(&text, None, None));
        }
        if let Ok(text) = std::fs::read_to_string(base.with_extension("txt")) {
            return Some(Lyrics::from_plain(&text, None, None));
        }
    }
    None
}

fn memory() -> &'static Mutex<HashMap<String, (Lyrics, usize)>> {
    static M: OnceLock<Mutex<HashMap<String, (Lyrics, usize)>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

fn remember(key: &str, lyrics: &Lyrics, index: usize) {
    if let Ok(mut m) = memory().lock() {
        if m.len() >= CACHE_LIMIT {
            m.clear();
        }
        m.insert(key.to_string(), (lyrics.clone(), index));
    }
}

fn recall(key: &str) -> Option<(Lyrics, usize)> {
    if let Some(found) = memory().lock().ok().and_then(|m| m.get(key).cloned()) {
        return Some(found);
    }
    let stored = lyrics_cache::read(key)?;
    remember(key, &stored.0, stored.1);
    Some(stored)
}

/// Текст песни. Никогда не падает: при сбое сети вернёт Error.
pub async fn find(title: String, artist: Option<String>, album: Option<String>, duration: Duration) -> Lyrics {
    // Свои файлы не кэшируем: их могут положить, пока песня играет
    if let Some(local) = find_local(&title, artist.as_deref()) {
        return local;
    }
    let key = format!("{}\n{}\n{}", artist.as_deref().unwrap_or(""), title, duration.as_secs());
    if let Some((l, _)) = recall(&key) {
        return l;
    }

    let (synced, plain_fallback) = fetch_timed(&title, artist.as_deref(), album.as_deref(), duration).await;
    let lyrics = match synced {
        Some(s) if rank(&s) < TRANSLIT_PENALTY => s,
        synced => {
            // Синхронного текста нет (или только транслит) — ищем обычный
            let plain = fetch_plain_tier(&title, artist.as_deref(), plain_fallback).await;
            match synced {
                Some(s) if matches!(plain.status, Status::NotFound | Status::Error) || rank(&s) <= rank(&plain) => s,
                s => {
                    if !matches!(plain.status, Status::NotFound | Status::Error) {
                        plain
                    } else {
                        s.unwrap_or(plain)
                    }
                }
            }
        }
    };
    if lyrics.status != Status::Error {
        remember(&key, &lyrics, 0);
        lyrics_cache::write(&key, &lyrics, 0);
    }
    lyrics
}

/// Выбор между прочтениями ролика: лучший текст с подходящей длительностью, среди равных — раньше по порядку;
/// потом любой найденный текст без меток времени.
fn pick_video(results: &[Option<Vec<LrcLibTrack>>], seconds: f64, candidates: &[(String, Option<String>)]) -> Option<(Lyrics, usize)> {
    let primary = candidates.iter().find_map(|(_, a)| a.clone().filter(|a| !a.trim().is_empty()));
    let mut best: Option<(Lyrics, usize, i32)> = None;
    for (i, r) in results.iter().enumerate() {
        let (title, artist) = candidates.get(i).map(|(t, a)| (Some(t.as_str()), a.clone())).unwrap_or((None, None));
        let effective = artist.or_else(|| primary.clone());
        if let Some(found) = r {
            if let Some((l, rk)) = pick_track(found, seconds, VIDEO_DURATION_TOLERANCE, title, effective.as_deref()) {
                if best.as_ref().map(|b| rk < b.2).unwrap_or(true) {
                    best = Some((l, i, rk));
                }
            }
        }
    }
    if let Some((l, i, _)) = best {
        return Some((l, i));
    }
    // Длительность не совпала ни у кого (длинный клип, live) — синхронизация не сойдётся, но сам текст верный
    for (i, r) in results.iter().enumerate() {
        let (title, artist) = candidates.get(i).map(|(t, a)| (Some(t.as_str()), a.clone())).unwrap_or((None, None));
        let effective = artist.or_else(|| primary.clone());
        let Some(found) = r else { continue };
        let plain = found
            .iter()
            .filter(|t| is_candidate_match(t, title, effective.as_deref()))
            .filter_map(to_lyrics)
            .min_by_key(rank);
        if let Some(p) = plain {
            let p = if p.status == Status::Synced { Lyrics::from_plain(&join_text(&p.lines), None, None) } else { p };
            return Some((p, i));
        }
    }
    None
}

/// Текст для ролика YouTube. Название ролика редко совпадает с названием песни, поэтому прочтений несколько
/// (самое вероятное первым). Возвращает текст и номер прочтения, по которому он нашёлся.
pub async fn find_video(candidates: Vec<(String, Option<String>)>, duration: Duration) -> (Lyrics, usize) {
    for (i, (t, a)) in candidates.iter().enumerate() {
        if let Some(local) = find_local(t, a.as_deref()) {
            return (local, i);
        }
    }
    if candidates.is_empty() {
        return (Lyrics::not_found(), 0);
    }
    let seconds = duration.as_secs_f64().round();
    let key = format!(
        "{}\n{}",
        seconds as i64,
        candidates.iter().map(|(t, a)| format!("{}\t{}", a.as_deref().unwrap_or(""), t)).collect::<Vec<_>>().join("\n")
    );
    if let Some((l, i)) = recall(&key) {
        if i < candidates.len() {
            return (l, i);
        }
    }

    // Первая очередь: поиск lrclib по всем прочтениям сразу
    let results: Vec<Option<Vec<LrcLibTrack>>> =
        futures::future::join_all(candidates.iter().map(|(t, a)| search_candidate(t.clone(), a.clone()))).await;
    let any_lrclib_failed = results.iter().any(|r| r.is_none());

    let result: (Lyrics, usize) = if let Some((picked, index)) = pick_video(&results, seconds, &candidates) {
        let mut result = (picked.clone(), index);
        // На lrclib только транслит — оригинал часто есть на NetEase или Genius
        if rank(&picked) >= TRANSLIT_PENALTY {
            let (t, a) = &candidates[index];
            let second = fetch_second_tier(t, a.as_deref(), seconds, VIDEO_DURATION_TOLERANCE).await;
            if matches!(second.status, Status::Synced | Status::Plain) && rank(&second) < rank(&picked) {
                result = (second, index);
            }
        }
        result
    } else {
        // lrclib не нашёл — Kugou для главного прочтения
        let (top_t, top_a) = candidates[0].clone();
        let kugou = fetch_kugou(top_t, top_a, seconds, VIDEO_DURATION_TOLERANCE).await;
        if kugou.status == Status::Synced && rank(&kugou) < TRANSLIT_PENALTY {
            (kugou, 0)
        } else {
            // Нет синхронизации — резервные базы
            let mut texts: Vec<Lyrics> = futures::future::join_all(candidates.iter().map(|(t, a)| {
                let (t, a) = (t.clone(), a.clone());
                async move {
                    if has_cyrillic(Some(&t)) || has_cyrillic(a.as_deref()) {
                        fetch_text_lyrics(t, a).await
                    } else {
                        fetch_ovh(t, a).await
                    }
                }
            }))
            .await;
            let mut found = texts.iter().position(|l| matches!(l.status, Status::Plain | Status::Instrumental));
            if found.is_none() {
                // И там нет — вторая очередь, до двух лучших прочтений
                let top: Vec<_> = candidates.iter().take(2).cloned().collect();
                let second: Vec<Lyrics> =
                    futures::future::join_all(top.iter().map(|(t, a)| async move { fetch_plain_tier(t, a.as_deref(), None).await })).await;
                found = second.iter().position(|l| l.status == Status::Synced);
                if found.is_none() {
                    found = second.iter().position(|l| matches!(l.status, Status::Plain | Status::Instrumental));
                }
                texts = texts.into_iter().take(top.len()).zip(second).map(|(ovh, s)| if s.status == Status::Error { ovh } else { s }).collect();
            }
            let any_failed = any_lrclib_failed && texts.iter().all(|l| l.status != Status::NotFound);
            match found {
                Some(i) => (texts[i].clone(), i),
                None => (if any_failed { Lyrics::error() } else { Lyrics::not_found() }, 0),
            }
        }
    };

    // «Не нашлось», когда часть запросов к lrclib сорвалась, тоже не запоминаем
    let status = result.0.status;
    if status != Status::Error && !(status == Status::NotFound && any_lrclib_failed) {
        remember(&key, &result.0, result.1);
        lyrics_cache::write(&key, &result.0, result.1);
    }
    result
}
