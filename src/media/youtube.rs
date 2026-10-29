//! Сведения о ролике с самого YouTube. Браузер сообщает только название, канал и кадр размером со спичечный
//! коробок (150×83), а id ролика — нет, поэтому ролик ищем по названию и каналу и сверяем с кадром из браузера.
//! Отсюда берутся песня из описания ролика и кадр в полном размере — для клипов, которых нет в каталогах обложек.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures::future::{FutureExt, Shared};
use regex::Regex;

use super::{escape, youtube as client};

/// Песня, которую YouTube указал в описании ролика (блок «Музыка»).
#[derive(Clone, Debug)]
pub struct YouTubeSong {
    pub title: String,
    pub artist: Option<String>,
}

const CANDIDATES: usize = 6;
/// Средняя разница цвета (0–255), при которой картинки ещё считаются одним кадром.
const MAX_DIFFERENCE: f64 = 28.0;
const CACHE_LIMIT: usize = 25;

type Pending<T> = Shared<Pin<Box<dyn Future<Output = Result<Option<T>, ()>> + Send>>>;

struct Cache<T: Clone + Send + Sync + 'static> {
    map: Mutex<HashMap<String, Pending<T>>>,
}

impl<T: Clone + Send + Sync + 'static> Cache<T> {
    fn new() -> Self {
        Self { map: Mutex::new(HashMap::new()) }
    }

    /// Один запрос на ключ: обои и поиск обложки спрашивают про один ролик одновременно. Ошибку (нет сети,
    /// YouTube поменял страницу) не запоминаем — спросим ещё раз.
    async fn get<F>(&self, key: String, load: impl FnOnce() -> F) -> Option<T>
    where
        F: Future<Output = Result<Option<T>, ()>> + Send + 'static,
    {
        let fut = {
            let mut map = self.map.lock().ok()?;
            match map.get(&key) {
                Some(f) => f.clone(),
                None => {
                    if map.len() >= CACHE_LIMIT {
                        map.clear();
                    }
                    let f: Pin<Box<dyn Future<Output = Result<Option<T>, ()>> + Send>> = Box::pin(load());
                    let shared = f.shared();
                    map.insert(key.clone(), shared.clone());
                    shared
                }
            }
        };
        match fut.await {
            Ok(v) => v,
            Err(()) => {
                if let Ok(mut map) = self.map.lock() {
                    map.remove(&key);
                }
                None
            }
        }
    }
}

fn ids() -> &'static Cache<String> {
    static C: OnceLock<Cache<String>> = OnceLock::new();
    C.get_or_init(Cache::new)
}
fn songs() -> &'static Cache<YouTubeSong> {
    static C: OnceLock<Cache<YouTubeSong>> = OnceLock::new();
    C.get_or_init(Cache::new)
}
fn covers() -> &'static Cache<Arc<Vec<u8>>> {
    static C: OnceLock<Cache<Arc<Vec<u8>>>> = OnceLock::new();
    C.get_or_init(Cache::new)
}

const JSON_TEXT: &str = r#""((?:[^"\\]|\\.)*)""#;

fn rx(cell: &'static OnceLock<Regex>, p: String) -> &'static Regex {
    cell.get_or_init(|| Regex::new(&p).expect("regex"))
}
fn search_result() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    rx(&R, r#""videoRenderer":\{"videoId":"([\w-]{11})""#.to_string())
}
fn result_title() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    rx(&R, format!(r#""title":\{{"runs":\[\{{"text":{JSON_TEXT}"#))
}
fn card_title() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    rx(&R, format!(r#""title":{JSON_TEXT}"#))
}
fn card_subtitle() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    rx(&R, format!(r#""subtitle":{JSON_TEXT}"#))
}

fn key(title: &str, channel: Option<&str>) -> String {
    format!("{}\n{}", channel.unwrap_or(""), title)
}

/// Песня из описания ролика. YouTube сам опознаёт музыку в роликах и пишет её в описании блоком «Музыка» —
/// раз он там есть, это песня, даже если её нет в базах текстов и обложек.
pub async fn find_song(title: &str, channel: Option<&str>, thumbnail: Option<Arc<Vec<u8>>>) -> Option<YouTubeSong> {
    let (t, c) = (title.to_string(), channel.map(String::from));
    songs()
        .get(key(title, channel), move || async move {
            let Some(id) = find_video_id(&t, c.as_deref(), thumbnail).await else { return Ok(None) };
            let page = async {
                let r = client().get(format!("https://www.youtube.com/watch?v={id}")).send().await.ok()?;
                if !r.status().is_success() {
                    return None;
                }
                r.text().await.ok()
            };
            match tokio::time::timeout(Duration::from_secs_f32(2.5), page).await {
                Ok(Some(html)) => Ok(parse_music_section(&html)),
                _ => Ok(None),
            }
        })
        .await
}

/// Кадр ролика 1280×720 (или 480×360, если большого нет). None — ролик не нашёлся.
pub async fn find_cover(title: &str, channel: Option<&str>, thumbnail: Option<Arc<Vec<u8>>>) -> Option<Arc<Vec<u8>>> {
    let (t, c) = (title.to_string(), channel.map(String::from));
    covers()
        .get(key(title, channel), move || async move {
            let Some(id) = find_video_id(&t, c.as_deref(), thumbnail).await else { return Ok(None) };
            // maxres есть не у всех роликов; hq — с чёрными полосами, но всё равно лучше крошечного
            if let Some(b) = get_bytes(&format!("https://i.ytimg.com/vi/{id}/maxresdefault.jpg")).await {
                return Ok(Some(Arc::new(b)));
            }
            Ok(get_bytes(&format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")).await.map(Arc::new))
        })
        .await
}

/// Ищет ролик в выдаче YouTube: если есть кадр из браузера — самый похожий на него, иначе первый с тем же названием.
async fn find_video_id(title: &str, channel: Option<&str>, thumbnail: Option<Arc<Vec<u8>>>) -> Option<String> {
    let (t, c) = (title.to_string(), channel.map(String::from));
    ids()
        .get(key(title, channel), move || async move {
            let q = escape(&match &c {
                Some(c) => format!("{t} {c}"),
                None => t.clone(),
            });
            let r = client().get(format!("https://www.youtube.com/results?search_query={q}")).send().await.map_err(|_| ())?;
            let html = r.text().await.map_err(|_| ())?;
            let results: Vec<(String, String)> = parse_search(&html).into_iter().take(CANDIDATES).collect();
            if results.is_empty() {
                return Ok(None);
            }
            if let Some(reference) = thumbnail.as_deref().and_then(|t| fingerprint(t)) {
                let previews = futures::future::join_all(results.iter().map(|(id, _)| {
                    let id = id.clone();
                    async move {
                        let bytes = get_bytes(&format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg")).await;
                        (id, bytes)
                    }
                }))
                .await;
                let mut best: Option<String> = None;
                let mut best_diff = MAX_DIFFERENCE;
                for (id, preview) in previews {
                    let Some(candidate) = preview.as_deref().and_then(fingerprint) else { continue };
                    let d = difference(&reference, &candidate);
                    if d < best_diff {
                        best_diff = d;
                        best = Some(id);
                    }
                }
                if best.is_some() {
                    return Ok(best);
                }
            }
            let normalized = normalize(&t);
            Ok(results
                .iter()
                .find(|(_, rt)| {
                    let n = normalize(rt);
                    n == normalized || (normalized.chars().count() >= 4 && (n.contains(&normalized) || normalized.contains(&n)))
                })
                .map(|(id, _)| id.clone()))
        })
        .await
}

/// Ролики из выдачи поиска: id и название.
pub fn parse_search(html: &str) -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for m in search_result().captures_iter(html) {
        let whole = m.get(0).unwrap();
        let id = m[1].to_string();
        if !seen.insert(id.clone()) {
            continue;
        }
        // Название — первое после id, в пределах этого же результата
        let start = whole.start();
        let mut end = (start + 4000).min(html.len());
        while !html.is_char_boundary(end) {
            end -= 1;
        }
        let title = result_title().captures(&html[start..end]).map(|c| unescape(&c[1])).unwrap_or_default();
        out.push((id, title));
    }
    out
}

/// Первая песня из блока «Музыка» в описании ролика; None — блока нет.
pub fn parse_music_section(html: &str) -> Option<YouTubeSong> {
    const MARK: &str = "\"horizontalCardListRenderer\"";
    let mut start = 0;
    while let Some(found) = html[start..].find(MARK) {
        let at = start + found;
        let next = html[at + 1..].find(MARK).map(|n| at + 1 + n);
        let mut end = next.unwrap_or_else(|| (at + 20000).min(html.len()));
        while !html.is_char_boundary(end) {
            end -= 1;
        }
        let section = &html[at..end];
        start = at + 1;
        // Такие же карточки бывают у мест, людей и игр — у музыки значок MUSIC
        if !section.contains("\"iconName\":\"MUSIC\"") {
            continue;
        }
        let Some(card) = section.find("\"videoAttributeViewModel\"") else { continue };
        let rest = &section[card..];
        let Some(title) = card_title().captures(rest) else { continue };
        let name = unescape(&title[1]).trim().to_string();
        if name.is_empty() {
            continue;
        }
        let artist = card_subtitle().captures(rest).map(|c| unescape(&c[1]).trim().to_string()).filter(|s| !s.is_empty());
        return Some(YouTubeSong { title: name, artist });
    }
    None
}

fn unescape(json: &str) -> String {
    serde_json::from_str::<String>(&format!("\"{json}\"")).unwrap_or_else(|_| json.to_string())
}

fn normalize(s: &str) -> String {
    super::text::multi_space().replace_all(&s.trim().to_lowercase(), " ").replace(['\t', '\n'], " ")
}

async fn get_bytes(url: &str) -> Option<Vec<u8>> {
    let r = client().get(url).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    r.bytes().await.ok().map(|b| b.to_vec())
}

/// Картинка, ужатая до 16×9: для сравнения кадров хватает с запасом.
fn fingerprint(image: &[u8]) -> Option<Vec<[u8; 3]>> {
    let img = image::load_from_memory(image).ok()?.to_rgb8();
    let (w, h) = (img.width() as f64, img.height() as f64);
    if w < 2.0 || h < 2.0 {
        return None;
    }
    // Кадр из браузера уже 16:9, а у YouTube бывают 4:3 с полосами — берём середину
    let aspect = w / h;
    let (cx, cy, cw, ch) = if aspect > 16.0 / 9.0 {
        let cw = h * 16.0 / 9.0;
        ((w - cw) / 2.0, 0.0, cw, h)
    } else {
        let ch = w * 9.0 / 16.0;
        (0.0, (h - ch) / 2.0, w, ch)
    };
    let crop = image::imageops::crop_imm(&img, cx as u32, cy as u32, cw.max(1.0) as u32, ch.max(1.0) as u32).to_image();
    let small = image::imageops::resize(&crop, 16, 9, image::imageops::FilterType::Triangle);
    Some(small.pixels().map(|p| [p[0], p[1], p[2]]).collect())
}

/// Средняя разница по каналам: по одной яркости разные картинки бывают слишком похожи.
fn difference(a: &[[u8; 3]], b: &[[u8; 3]]) -> f64 {
    let sum: f64 = a
        .iter()
        .zip(b)
        .map(|(p, q)| {
            ((p[0] as i32 - q[0] as i32).abs() + (p[1] as i32 - q[1] as i32).abs() + (p[2] as i32 - q[2] as i32).abs()) as f64
                / 3.0
        })
        .sum();
    sum / (16.0 * 9.0)
}
