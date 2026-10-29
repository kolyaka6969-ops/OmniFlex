//! Что играет и текст песни: медиа-сеансы Windows, обложки из каталогов, тексты из открытых баз.

pub mod anime;
pub mod browser;
pub mod cover;
pub mod lyrics;
pub mod lyrics_cache;
pub mod player;
pub mod session;
pub mod text;
pub mod youtube;

use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

/// Плеер, из которого пришла песня.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MusicSource {
    #[default]
    None,
    Spotify,
    YandexMusic,
    YouTubeMusic,
    YouTube,
}

/// Что играет. Позиция — на момент position_time, во время игры её досчитываем по часам.
#[derive(Clone, Debug)]
pub struct NowPlaying {
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub cover: Option<Arc<Vec<u8>>>,
    pub is_playing: bool,
    pub position: Duration,
    pub position_time: SystemTime,
    pub duration: Duration,
    pub source: MusicSource,
    /// У роликов YouTube — название ролика как есть, с пометками вроде «(Official Video)».
    pub video_title: Option<String>,
    /// Обложка — кадр ролика YouTube: официальной в каталогах не нашлось.
    pub cover_is_frame: bool,
}

impl NowPlaying {
    /// Один и тот же трек, даже если сменились позиция или обложка.
    pub fn key(&self) -> String {
        format!(
            "{}\n{}\n{}",
            self.artist.as_deref().unwrap_or(""),
            self.title,
            self.album.as_deref().unwrap_or("")
        )
    }

    pub fn position_at(&self, now: SystemTime) -> Duration {
        extrapolate(self.position, self.position_time, self.is_playing, self.duration, now)
    }

    pub fn same_cover(&self, other: &NowPlaying) -> bool {
        match (&self.cover, &other.cover) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

pub fn extrapolate(position: Duration, at: SystemTime, playing: bool, duration: Duration, now: SystemTime) -> Duration {
    let pos = if playing {
        match now.duration_since(at) {
            Ok(d) => position + d,
            Err(e) => position.saturating_sub(e.duration()),
        }
    } else {
        position
    };
    if duration > Duration::ZERO && pos > duration {
        duration
    } else {
        pos
    }
}

// ---------- Сеть ----------

pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(3)
            .enable_all()
            .thread_name("OmniFlex net")
            .build()
            .expect("tokio runtime")
    })
}

fn builder(timeout: f32) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(Duration::from_secs_f32(timeout))
        .connect_timeout(Duration::from_secs(3))
        .pool_idle_timeout(Duration::from_secs(90))
        .gzip(true)
        .brotli(true)
        .deflate(true)
}

fn agent() -> String {
    format!("OmniFlex/{} (+https://github.com/kolyaka6969-ops/OmniFlex)", crate::util::VERSION)
}

/// lrclib просит подписывать запросы названием программы.
pub fn lrclib() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| builder(4.0).user_agent(agent()).cookie_store(true).build().expect("http"))
}

/// Резервные базы текстов и каталоги обложек.
pub fn fallback() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        builder(4.0).user_agent(format!("OmniFlex/{}", crate::util::VERSION)).cookie_store(true).build().expect("http")
    })
}

/// NetEase ставит cookie на первом ответе и с ними отдаёт пустой поиск — ему (и Kugou) отдельный клиент без cookie.
pub fn no_cookies() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| builder(4.0).user_agent(format!("OmniFlex/{}", crate::util::VERSION)).build().expect("http"))
}

/// YouTube: как браузер, без cookie, с согласием на куки (иначе в Европе приходит страница согласия).
pub fn youtube() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::ACCEPT_LANGUAGE, "ru,en;q=0.8".parse().unwrap());
        headers.insert(reqwest::header::COOKIE, "SOCS=CAI".parse().unwrap());
        builder(4.0)
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36",
            )
            .default_headers(headers)
            .build()
            .expect("http")
    })
}

/// AnimeThemes отвечает медленно.
pub fn slow() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| builder(20.0).user_agent(format!("OmniFlex/{}", crate::util::VERSION)).build().expect("http"))
}

/// Строка из JSON-объекта.
pub fn str_of<'a>(v: &'a serde_json::Value, name: &str) -> Option<&'a str> {
    v.get(name).and_then(|x| x.as_str())
}

/// Uri.EscapeDataString.
pub fn escape(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}
