//! Обложка альбома по исполнителю и названию. Нужна для YouTube: браузер отдаёт в медиа-сеансе кадр из ролика,
//! а на рабочем столе должна висеть обложка трека. Каталоги: Deezer, Apple Music (iTunes) и запасной — Genius.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::Value;

use super::text::{norm, score};
use super::{escape, fallback, str_of};

const CACHE_LIMIT: usize = 30;
/// Три каталога по очереди на каждое прочтение.
const BUDGET: Duration = Duration::from_secs(10);

pub type Art = Option<Arc<Vec<u8>>>;

fn cache() -> &'static Mutex<HashMap<String, Art>> {
    static C: OnceLock<Mutex<HashMap<String, Art>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Обложка или None; failed — поиск был неполным (каталог не ответил), и пустой ответ не значит «обложки нет».
pub async fn look_up(candidates: &[(String, Option<String>)]) -> (Art, bool) {
    let primary = candidates.iter().find_map(|(_, a)| a.clone().filter(|a| !a.trim().is_empty()));
    let work = async {
        let mut failed = false;
        for (title, artist) in candidates {
            let (art, one_failed) = find_one(title, artist.as_deref(), primary.as_deref()).await;
            if art.is_some() {
                return (art, false);
            }
            failed |= one_failed;
        }
        (None, failed)
    };
    match tokio::time::timeout(BUDGET, work).await {
        Ok(r) => r,
        Err(_) => (None, true),
    }
}

pub async fn find(candidates: &[(String, Option<String>)]) -> Art {
    look_up(candidates).await.0
}

async fn find_one(title: &str, artist: Option<&str>, primary: Option<&str>) -> (Art, bool) {
    let effective = artist.filter(|a| !a.trim().is_empty()).or(primary);
    let key = format!("{}\n{}", norm(effective), norm(Some(title)));
    if let Some(cached) = cache().lock().ok().and_then(|c| c.get(&key).cloned()) {
        return (cached, false);
    }

    // Deezer и iTunes опрашиваем параллельно: отвечают за 150–300 мс
    let (deezer, itunes) = tokio::join!(from_deezer(title, artist, effective), from_itunes(title, artist, effective));
    let (deezer_art, deezer_score, deezer_failed) = deezer;
    let (itunes_art, itunes_score, itunes_failed) = itunes;
    let mut art = if deezer_art.is_some() && deezer_score >= itunes_score {
        deezer_art
    } else if itunes_art.is_some() {
        itunes_art
    } else {
        deezer_art
    };
    let mut failed = deezer_failed || itunes_failed;

    // Если ни Deezer, ни iTunes не помогли — запасной каталог Genius
    if art.is_none() {
        match tokio::time::timeout(Duration::from_secs(3), from_genius(title, artist, effective)).await {
            Ok(Ok(found)) => art = found,
            _ => failed = true,
        }
    }

    // Отказ сервиса не запоминаем: за часы прослушивания каталоги порой упираются в лимит запросов
    if art.is_some() || !failed {
        if let Ok(mut c) = cache().lock() {
            if c.len() >= CACHE_LIMIT {
                c.clear();
            }
            c.insert(key, art.clone());
        }
    }
    let failed = art.is_none() && failed;
    (art, failed)
}

fn query(title: &str, artist: Option<&str>, effective: Option<&str>) -> String {
    let search_artist = artist.filter(|a| !a.trim().is_empty()).or(effective);
    escape(&match search_artist {
        Some(a) if !a.trim().is_empty() => format!("{a} {title}"),
        _ => title.to_string(),
    })
}

fn min_score(effective: Option<&str>) -> i32 {
    if effective.map(|a| a.trim().is_empty()).unwrap_or(true) {
        3
    } else {
        5
    }
}

async fn get_json(url: &str) -> Result<Value, ()> {
    let response = fallback().get(url).send().await.map_err(|_| ())?;
    if !response.status().is_success() {
        return Err(());
    }
    response.json::<Value>().await.map_err(|_| ())
}

async fn from_deezer(title: &str, artist: Option<&str>, effective: Option<&str>) -> (Art, i32, bool) {
    let url = format!("https://api.deezer.com/search?q={}&limit=6", query(title, artist, effective));
    let Ok(doc) = get_json(&url).await else { return (None, 0, true) };
    // Превышение лимита Deezer отдаёт с кодом 200 и полем error
    if doc.get("error").is_some() {
        return (None, 0, true);
    }
    let Some(data) = doc.get("data").and_then(|d| d.as_array()) else { return (None, 0, false) };
    let min = min_score(effective);
    let mut best = 0;
    let mut best_url: Option<String> = None;
    for item in data {
        let s = score(title, effective, str_of(item, "title"), item.get("artist").and_then(|a| str_of(a, "name")));
        let Some(album) = item.get("album") else { continue };
        if s <= best || s < min {
            continue;
        }
        best = s;
        best_url = str_of(album, "cover_xl").or(str_of(album, "cover_big")).or(str_of(album, "cover_medium")).map(String::from);
    }
    match best_url {
        Some(u) => match download(&u).await {
            Some(art) => (Some(art), best, false),
            None => (None, 0, false),
        },
        None => (None, 0, false),
    }
}

async fn from_itunes(title: &str, artist: Option<&str>, effective: Option<&str>) -> (Art, i32, bool) {
    let url = format!(
        "https://itunes.apple.com/search?term={}&media=music&entity=song&limit=8",
        query(title, artist, effective)
    );
    let Ok(doc) = get_json(&url).await else { return (None, 0, true) };
    let Some(results) = doc.get("results").and_then(|d| d.as_array()) else { return (None, 0, false) };
    let min = min_score(effective);
    let mut best = 0;
    let mut best_url: Option<String> = None;
    for item in results {
        let s = score(title, effective, str_of(item, "trackName"), str_of(item, "artistName"));
        let Some(art) = str_of(item, "artworkUrl100") else { continue };
        if s <= best || s < min {
            continue;
        }
        best = s;
        // 100x100bb.jpg — превью; тот же файл в 600x600bb.jpg заметно приятнее на обоях
        best_url = Some(art.replace("100x100bb", "600x600bb"));
    }
    match best_url {
        Some(u) => match download(&u).await {
            Some(art) => (Some(art), best, false),
            None => (None, 0, false),
        },
        None => (None, 0, false),
    }
}

/// Genius: обложки есть почти у всех песен, у которых там есть текст. Берём только песню с тем же названием и исполнителем.
async fn from_genius(title: &str, artist: Option<&str>, effective: Option<&str>) -> Result<Art, ()> {
    let url = format!("https://genius.com/api/search/song?q={}&per_page=5", query(title, artist, effective));
    let doc = get_json(&url).await?;
    let Some(hits) = doc
        .get("response")
        .and_then(|r| r.get("sections"))
        .and_then(|s| s.get(0))
        .and_then(|s| s.get("hits"))
        .and_then(|h| h.as_array())
    else {
        return Ok(None);
    };
    let min = min_score(effective);
    let mut best = 0;
    let mut best_url: Option<String> = None;
    for hit in hits {
        let Some(result) = hit.get("result") else { continue };
        let found_artist = str_of(result, "artist_names").or(result.get("primary_artist").and_then(|p| str_of(p, "name")));
        let s = score(title, effective, str_of(result, "title"), found_artist);
        let Some(art) = str_of(result, "song_art_image_url") else { continue };
        // У песен без своей обложки Genius подставляет общую заглушку
        if s <= best || s < min || art.to_lowercase().contains("default_cover") {
            continue;
        }
        best = s;
        best_url = Some(art.to_string());
    }
    if best < min {
        return Ok(None);
    }
    match best_url {
        Some(u) => Ok(download(&u).await),
        None => Ok(None),
    }
}

pub async fn download(url: &str) -> Option<Arc<Vec<u8>>> {
    let response = fallback().get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    if bytes.len() > 512 && bytes.len() < 8 * 1024 * 1024 && is_image(&bytes) {
        Some(Arc::new(bytes.to_vec()))
    } else {
        None
    }
}

fn is_image(b: &[u8]) -> bool {
    (b.len() > 2 && b[0] == 0xFF && b[1] == 0xD8) || (b.len() > 8 && b[0] == 0x89 && b[1] == 0x50 && b[2] == 0x4E && b[3] == 0x47)
}
