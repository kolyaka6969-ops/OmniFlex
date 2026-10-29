//! Плеер острова: любой медиа-сеанс Windows (а не только музыкальные, как у обоев). Когда открыто несколько
//! плееров, остров показывает выбранный в настройках, иначе — тот, что играет. Живёт в потоке ядра;
//! острову достаётся снимок через `state()`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::Foundation::{EventRegistrationToken, TypedEventHandler};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};

use super::lyrics::{self, Lyrics};
use super::session::{is_browser, read_stream};
use crate::core::{self, Event};

#[derive(Clone, Debug, Default)]
pub struct Track {
    pub aumid: String,
    pub app: String,
    pub title: String,
    pub artist: String,
    pub cover: Option<Arc<Vec<u8>>>,
    pub playing: bool,
    pub position: Duration,
    pub position_time: Option<SystemTime>,
    pub duration: Duration,
}

impl Track {
    pub fn key(&self) -> String {
        format!("{}\n{}\n{}", self.aumid, self.artist, self.title)
    }

    pub fn position_now(&self) -> Duration {
        let at = self.position_time.unwrap_or_else(SystemTime::now);
        super::extrapolate(self.position, at, self.playing, self.duration, SystemTime::now())
    }
}

#[derive(Clone, Debug, Default)]
pub struct PlayerInfo {
    pub aumid: String,
    pub app: String,
    pub playing: bool,
}

#[derive(Clone, Default)]
pub struct State {
    pub track: Option<Track>,
    pub players: Vec<PlayerInfo>,
    /// Текст песни для track.key().
    pub lyrics: Option<(String, Lyrics)>,
    pub version: u64,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

pub fn state() -> State {
    STATE.lock().ok().and_then(|s| s.clone()).unwrap_or_default()
}

fn publish(f: impl FnOnce(&mut State)) {
    if let Ok(mut s) = STATE.lock() {
        let st = s.get_or_insert_with(State::default);
        f(st);
        st.version += 1;
    }
    crate::shell::post(crate::island::player_changed);
}

#[derive(Clone, Debug)]
pub enum Command {
    PlayPause,
    Next,
    Previous,
    /// Доля трека 0..1.
    Seek(f64),
    /// Сменился выбранный плеер или включили текст песни — перечитать.
    Reload,
}

pub fn command(cmd: Command) {
    core::post(Event::Player(cmd));
}

/// Понятное имя плеера по AUMID: «Spotify», «Chrome», «Яндекс Музыка».
pub fn app_name(aumid: &str) -> String {
    let l = aumid.to_lowercase();
    let known = [
        ("spotify", "Spotify"),
        ("yandex.music", "Яндекс Музыка"),
        ("yandexmusic", "Яндекс Музыка"),
        ("chrome", "Chrome"),
        ("msedge", "Edge"),
        ("firefox", "Firefox"),
        ("opera", "Opera"),
        ("browser", "Яндекс Браузер"),
        ("vivaldi", "Vivaldi"),
        ("brave", "Brave"),
        ("zune", "Медиаплеер"),
        ("media.player", "Медиаплеер"),
        ("vlc", "VLC"),
        ("aimp", "AIMP"),
        ("foobar", "foobar2000"),
        ("telegram", "Telegram"),
        ("discord", "Discord"),
        ("applemusic", "Apple Music"),
        ("itunes", "iTunes"),
        ("deezer", "Deezer"),
        ("soundcloud", "SoundCloud"),
    ];
    if let Some((_, name)) = known.iter().find(|(k, _)| l.contains(k)) {
        return name.to_string();
    }
    let tail = aumid.rsplit('!').next().unwrap_or(aumid);
    let stem = crate::win::file_stem(tail);
    if stem.is_empty() {
        aumid.to_string()
    } else {
        stem
    }
}

fn aumid(s: &Session) -> String {
    s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default()
}

fn is_playing(s: &Session) -> bool {
    s.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()).map(|st| st == PlaybackStatus::Playing).unwrap_or(false)
}

fn to_system_time(ticks: i64) -> SystemTime {
    const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
    let unix = ticks - EPOCH_DIFF;
    if unix <= 0 {
        return SystemTime::now();
    }
    UNIX_EPOCH + Duration::from_nanos(unix as u64 * 100)
}

pub struct Player {
    manager: Option<Manager>,
    session: Option<Session>,
    tokens: Vec<(u8, EventRegistrationToken)>,
    watched: Vec<(Session, EventRegistrationToken)>,
    track: Option<Track>,
    lyrics_key: Option<String>,
    lyrics_task: Option<tokio::task::JoinHandle<()>>,
}

impl Player {
    pub fn new() -> Self {
        Self { manager: None, session: None, tokens: Vec::new(), watched: Vec::new(), track: None, lyrics_key: None, lyrics_task: None }
    }

    pub fn start(&mut self) {
        let Ok(manager) = Manager::RequestAsync().and_then(|op| op.get()) else { return };
        let _ = manager.SessionsChanged(&TypedEventHandler::new(|_, _| {
            core::post(Event::PlayerChanged);
            Ok(())
        }));
        let _ = manager.CurrentSessionChanged(&TypedEventHandler::new(|_, _| {
            core::post(Event::PlayerChanged);
            Ok(())
        }));
        self.manager = Some(manager);
        self.reload();
    }

    pub fn handle(&mut self, cmd: Command) {
        let Some(s) = self.session.clone() else {
            if matches!(cmd, Command::Reload) {
                self.reload();
            }
            return;
        };
        match cmd {
            Command::PlayPause => {
                let _ = s.TryTogglePlayPauseAsync();
            }
            Command::Next => {
                let _ = s.TrySkipNextAsync();
            }
            Command::Previous => {
                let _ = s.TrySkipPreviousAsync();
            }
            Command::Seek(f) => {
                if let Ok(t) = s.GetTimelineProperties() {
                    let start = t.StartTime().map(|d| d.Duration).unwrap_or(0);
                    let end = t.EndTime().map(|d| d.Duration).unwrap_or(0);
                    if end > start {
                        let pos = start + ((end - start) as f64 * f.clamp(0.0, 1.0)) as i64;
                        let _ = s.TryChangePlaybackPositionAsync(pos);
                        if let Some(track) = &mut self.track {
                            track.position = Duration::from_nanos(((pos - start).max(0) as u64) * 100);
                            track.position_time = Some(SystemTime::now());
                            let t = track.clone();
                            publish(|st| st.track = Some(t));
                        }
                    }
                }
            }
            Command::Reload => {
                self.lyrics_key = None;
                self.reload();
            }
        }
    }

    /// Выбирает сеанс и перечитывает всё о нём.
    pub fn reload(&mut self) {
        let Some(manager) = self.manager.clone() else { return };
        let sessions: Vec<Session> = manager.GetSessions().map(|l| l.into_iter().collect()).unwrap_or_default();
        self.watch(&sessions);
        let players: Vec<PlayerInfo> = sessions
            .iter()
            .map(|s| {
                let id = aumid(s);
                PlayerInfo { app: app_name(&id), aumid: id, playing: is_playing(s) }
            })
            .collect();
        let chosen = crate::settings::read(|s| s.island.player.clone());
        let pick = sessions
            .iter()
            .find(|s| !chosen.is_empty() && aumid(s).eq_ignore_ascii_case(&chosen))
            .cloned()
            .or_else(|| {
                // Сначала тот, что играет и уже был выбран, потом любой играющий, потом текущий по мнению Windows
                let current = self.session.as_ref().map(aumid);
                sessions
                    .iter()
                    .find(|s| is_playing(s) && Some(aumid(s)) == current)
                    .or_else(|| sessions.iter().find(|s| is_playing(s)))
                    .cloned()
                    .or_else(|| manager.GetCurrentSession().ok())
                    .or_else(|| sessions.iter().find(|s| Some(aumid(s)) == current).cloned())
            });
        let same = match (&pick, &self.session) {
            (Some(a), Some(b)) => aumid(a) == aumid(b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.unsubscribe();
            self.session = pick;
            self.subscribe();
        }
        self.read();
        publish(|st| st.players = players);
    }

    fn watch(&mut self, sessions: &[Session]) {
        self.watched.retain(|(w, token)| {
            let keep = sessions.iter().any(|s| aumid(s) == aumid(w));
            if !keep {
                let _ = w.RemovePlaybackInfoChanged(*token);
            }
            keep
        });
        for s in sessions {
            if self.watched.iter().any(|(w, _)| aumid(w) == aumid(s)) {
                continue;
            }
            if let Ok(token) = s.PlaybackInfoChanged(&TypedEventHandler::new(|_, _| {
                core::post(Event::PlayerChanged);
                Ok(())
            })) {
                self.watched.push((s.clone(), token));
            }
        }
    }

    fn subscribe(&mut self) {
        let Some(s) = &self.session else { return };
        if let Ok(t) = s.MediaPropertiesChanged(&TypedEventHandler::new(|_, _| {
            core::post(Event::PlayerRead);
            Ok(())
        })) {
            self.tokens.push((1, t));
        }
        if let Ok(t) = s.TimelinePropertiesChanged(&TypedEventHandler::new(|_, _| {
            core::post(Event::PlayerRead);
            Ok(())
        })) {
            self.tokens.push((2, t));
        }
    }

    fn unsubscribe(&mut self) {
        if let Some(s) = &self.session {
            for (kind, token) in self.tokens.drain(..) {
                let _ = if kind == 1 { s.RemoveMediaPropertiesChanged(token) } else { s.RemoveTimelinePropertiesChanged(token) };
            }
        }
        self.tokens.clear();
    }

    /// Перечитать трек выбранного сеанса.
    pub fn read(&mut self) {
        let Some(s) = self.session.clone() else {
            if self.track.take().is_some() {
                publish(|st| {
                    st.track = None;
                    st.lyrics = None;
                });
            }
            return;
        };
        let id = aumid(&s);
        let mut track = Track { app: app_name(&id), aumid: id, playing: is_playing(&s), ..Default::default() };
        let mut thumb = None;
        if let Ok(props) = s.TryGetMediaPropertiesAsync().and_then(|op| op.get()) {
            track.title = props.Title().map(|h| h.to_string()).unwrap_or_default();
            track.artist = props.Artist().map(|h| h.to_string()).unwrap_or_default();
            thumb = props.Thumbnail().ok();
        }
        if let Ok(t) = s.GetTimelineProperties() {
            let start = t.StartTime().map(|d| d.Duration).unwrap_or(0);
            let end = t.EndTime().map(|d| d.Duration).unwrap_or(0);
            let pos = t.Position().map(|d| d.Duration).unwrap_or(0);
            let at = t.LastUpdatedTime().map(|d| to_system_time(d.UniversalTime)).unwrap_or_else(|_| SystemTime::now());
            let now = SystemTime::now();
            track.duration = Duration::from_nanos(((end - start).max(0) as u64) * 100);
            track.position = Duration::from_nanos(((pos - start).max(0) as u64) * 100);
            track.position_time = Some(if at > now { now } else { at });
        }
        let old = self.track.clone();
        let same_track = old.as_ref().map(|o| o.key() == track.key()).unwrap_or(false);
        // Обложку перечитываем только у нового трека: браузер присылает её позже названия, поэтому и при пустой
        track.cover = match &old {
            Some(o) if same_track && o.cover.is_some() => o.cover.clone(),
            _ => thumb.as_ref().and_then(read_stream).map(Arc::new),
        };
        // Позиция без сведений о времени от плеера при паузе и продолжении: досчитываем по своим часам
        if let Some(o) = &old {
            if same_track && o.playing != track.playing && track.position.is_zero() {
                track.position = o.position_now();
                track.position_time = Some(SystemTime::now());
            }
        }
        self.track = Some(track.clone());
        let key = track.key();
        publish(|st| {
            if st.lyrics.as_ref().map(|(k, _)| k != &key).unwrap_or(false) {
                st.lyrics = None;
            }
            st.track = Some(track.clone());
        });
        self.fetch_lyrics(&track);
    }

    fn fetch_lyrics(&mut self, track: &Track) {
        let wanted = crate::settings::read(|s| s.island.lyrics);
        if !wanted || track.title.trim().is_empty() {
            return;
        }
        let key = track.key();
        // Длительность браузер присылает не сразу, а по ней базы различают версии
        let lk = format!("{key}\n{}", track.duration.as_secs());
        if self.lyrics_key.as_deref() == Some(&lk) {
            return;
        }
        self.lyrics_key = Some(lk);
        if let Some(t) = self.lyrics_task.take() {
            t.abort();
        }
        let (title, artist, duration, browser) = (track.title.clone(), track.artist.clone(), track.duration, is_browser(&track.aumid));
        self.lyrics_task = Some(super::runtime().spawn(async move {
            let artist = if artist.trim().is_empty() { None } else { Some(artist) };
            let found = if browser {
                let candidates: Vec<_> = super::text::candidates(&title, artist.as_deref()).into_iter().take(4).collect();
                lyrics::find_video(candidates, duration).await.0
            } else {
                lyrics::find(title, artist, None, duration).await
            };
            core::post(Event::PlayerLyrics { key, lyrics: found });
        }));
    }

    pub fn lyrics_found(&mut self, key: String, found: Lyrics) {
        if self.track.as_ref().map(|t| t.key()) != Some(key.clone()) {
            return;
        }
        publish(|st| st.lyrics = Some((key, found)));
    }

    pub fn shutdown(&mut self) {
        self.unsubscribe();
        for (w, token) in self.watched.drain(..) {
            let _ = w.RemovePlaybackInfoChanged(token);
        }
        self.session = None;
    }
}
