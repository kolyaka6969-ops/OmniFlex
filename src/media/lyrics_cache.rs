//! Найденные тексты на диске: песня, которую уже слушали, получает текст сразу, без запросов к базам — и после
//! перезапуска. «Не нашлось» хранится сутки (базы пополняются), найденный текст — месяц.
//! Файл: строка «omniflex-lyrics 1 <статус> <номер прочтения>», дальше строки «<метка в мс>\t<текст>».

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use super::lyrics::{clean_leading_metadata, Line, Lyrics, Status};

const HEADER: &str = "omniflex-lyrics 1";
const FOUND_LIFETIME: Duration = Duration::from_secs(30 * 24 * 3600);
const MISSING_LIFETIME: Duration = Duration::from_secs(24 * 3600);

static WRITES: AtomicU32 = AtomicU32::new(0);

fn folder() -> std::path::PathBuf {
    crate::util::data_dir().join("cache").join("lyrics")
}

fn path_for(key: &str) -> std::path::PathBuf {
    let hash = Sha256::digest(key.as_bytes());
    let hex: String = hash.iter().map(|b| format!("{b:02X}")).collect();
    folder().join(format!("{}.txt", &hex[..24]))
}

fn status_name(s: Status) -> &'static str {
    match s {
        Status::Loading => "Loading",
        Status::Synced => "Synced",
        Status::Plain => "Plain",
        Status::Instrumental => "Instrumental",
        Status::NotFound => "NotFound",
        Status::Error => "Error",
    }
}

pub fn read(key: &str) -> Option<(Lyrics, usize)> {
    let path = path_for(key);
    let meta = std::fs::metadata(&path).ok()?;
    let parsed = parse(&std::fs::read_to_string(&path).ok()?);
    let age = meta.modified().ok().and_then(|m| SystemTime::now().duration_since(m).ok()).unwrap_or_default();
    let lifetime = if parsed.as_ref().map(|p| p.0.status == Status::NotFound).unwrap_or(false) {
        MISSING_LIFETIME
    } else {
        FOUND_LIFETIME
    };
    if parsed.is_none() || age > lifetime {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    parsed
}

pub fn write(key: &str, lyrics: &Lyrics, index: usize) {
    if matches!(lyrics.status, Status::Error | Status::Loading) {
        return;
    }
    let _ = std::fs::create_dir_all(folder());
    let _ = std::fs::write(path_for(key), format(lyrics, index));
    // Изредка убираем устаревшее, чтобы папка не росла бесконечно
    if WRITES.fetch_add(1, Ordering::Relaxed) % 50 == 0 {
        prune();
    }
}

fn format(lyrics: &Lyrics, index: usize) -> String {
    let mut s = format!("{HEADER} {} {index}\n", status_name(lyrics.status));
    for line in lyrics.lines.iter() {
        s.push_str(&format!("{}\t{}\n", line.time.as_millis(), line.text.replace('\n', " ")));
    }
    s
}

fn parse(text: &str) -> Option<(Lyrics, usize)> {
    let mut rows = text.split('\n');
    let head: Vec<&str> = rows.next()?.split(' ').collect();
    if head.len() != 4 || format!("{} {}", head[0], head[1]) != HEADER {
        return None;
    }
    let status = match head[2] {
        "Synced" => Status::Synced,
        "Plain" => Status::Plain,
        "Instrumental" => Status::Instrumental,
        "NotFound" => Status::NotFound,
        _ => return None,
    };
    let index: usize = head[3].parse().ok()?;
    let mut lines = Vec::new();
    for row in rows {
        let Some(tab) = row.find('\t') else { continue };
        let Ok(ms) = row[..tab].parse::<u64>() else { continue };
        lines.push(Line { time: Duration::from_millis(ms), text: row[tab + 1..].to_string() });
    }
    if !lines.is_empty() {
        lines = clean_leading_metadata(lines, None, None);
    }
    let lyrics = match status {
        Status::NotFound => Lyrics::not_found(),
        Status::Instrumental => Lyrics::instrumental(),
        Status::Synced | Status::Plain if !lines.is_empty() => Lyrics::new(status, lines),
        _ => return None,
    };
    Some((lyrics, index))
}

fn prune() {
    let Ok(dir) = std::fs::read_dir(folder()) else { return };
    for entry in dir.flatten() {
        let old = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .map(|age| age > FOUND_LIFETIME)
            .unwrap_or(false);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
