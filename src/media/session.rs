//! Песня и обложка из системных медиа-сеансов Windows (те же, что у кнопок в панели громкости).
//! Работает в потоке ядра (core): события сеансов приходят из пула потоков и пересылаются туда же,
//! долгие запросы к каталогам обложек идут в сети и возвращаются событием.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::core::Interface;
use windows::Foundation::{EventRegistrationToken, IAsyncOperation, TypedEventHandler};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};

use super::browser::{BrowserMusic, BrowserMusicDetector, BROWSER_EXES};
use super::{anime, cover, text, youtube, MusicSource, NowPlaying};
use crate::core::{self, Event, Timer, Timers};
use crate::settings::MusicSourceMode;

/// Как часто проверяем, не открылась ли музыка в браузере.
const BROWSER_TICK: Duration = Duration::from_secs(4);
/// Вкладки браузера открываются и закрываются пачками — выбираем сеанс, когда всё уляжется.
const SESSIONS_DELAY: Duration = Duration::from_millis(300);
/// Каталоги обложек не ответили (лимит запросов, таймаут) — спросим ещё раз.
const COVER_RETRY: Duration = Duration::from_secs(30);
/// Обложка меньше этого размера на обоях выглядит мыльной.
const SHARP_COVER: u32 = 400;

type Bytes = Option<Arc<Vec<u8>>>;

pub struct MediaSession {
    manager: Option<Manager>,
    session: Option<Session>,
    session_tokens: Vec<(u8, EventRegistrationToken)>,
    watched: Vec<(Session, EventRegistrationToken)>,
    browser: BrowserMusicDetector,
    mode: MusicSourceMode,
    pub source: MusicSource,
    cover_key: Option<String>,
    catalog_key: Option<String>,
    video_title: Option<String>,
    video_channel: Option<String>,
    cover_is_frame: bool,
    catalog_cover: bool,
    thumb: Bytes,
    previous_thumb: Bytes,
    thumb_key: Option<String>,
    thumb_album: Option<String>,
    previous_album: Option<String>,
    suspect_cover: Bytes,
    refresh_version: u64,
    accept_any_timeline: bool,
    pub is_playing: bool,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub cover: Bytes,
    pub duration: Duration,
    pub position: Duration,
    pub position_time: SystemTime,
}

fn aumid(s: &Session) -> String {
    s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default()
}

fn same_session(a: Option<&Session>, b: Option<&Session>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.as_raw() == b.as_raw() || aumid(a).to_lowercase() == aumid(b).to_lowercase(),
        _ => false,
    }
}

pub fn is_spotify(aumid: &str) -> bool {
    aumid.to_lowercase().contains("spotify")
}

pub fn is_yandex_music_app(aumid: &str) -> bool {
    let l = aumid.to_lowercase();
    (l.contains("yandex") || l.contains("яндекс")) && (l.contains("music") || l.contains("музык"))
}

pub fn is_browser(aumid: &str) -> bool {
    if aumid.is_empty() {
        return false;
    }
    let id = if aumid.contains('\\') || aumid.contains('/') { crate::win::file_stem(aumid) } else { aumid.to_string() };
    let id = id.to_lowercase();
    BROWSER_EXES.iter().any(|b| id.contains(b))
}

fn to_source(b: BrowserMusic) -> MusicSource {
    match b {
        BrowserMusic::YandexMusic => MusicSource::YandexMusic,
        BrowserMusic::YouTubeMusic => MusicSource::YouTubeMusic,
        BrowserMusic::YouTube => MusicSource::YouTube,
        BrowserMusic::None => MusicSource::None,
    }
}

fn to_browser(s: MusicSource) -> BrowserMusic {
    match s {
        MusicSource::YandexMusic => BrowserMusic::YandexMusic,
        MusicSource::YouTubeMusic => BrowserMusic::YouTubeMusic,
        MusicSource::YouTube => BrowserMusic::YouTube,
        _ => BrowserMusic::None,
    }
}

/// Опознаёт плеер по AUMID. Сайт, открытый во вкладке, по AUMID не виден — его подсказывает детектор браузера.
pub fn classify(aumid: &str, mode: MusicSourceMode, browser: BrowserMusic) -> MusicSource {
    if is_spotify(aumid) {
        return if mode == MusicSourceMode::YandexMusic { MusicSource::None } else { MusicSource::Spotify };
    }
    if is_yandex_music_app(aumid) {
        return if mode == MusicSourceMode::Spotify { MusicSource::None } else { MusicSource::YandexMusic };
    }
    if !is_browser(aumid) {
        return MusicSource::None;
    }
    match mode {
        MusicSourceMode::Spotify => MusicSource::None,
        // Явный выбор источника — запасной путь: сайт опознать не всегда выходит, а песня играет
        MusicSourceMode::YandexMusic => MusicSource::YandexMusic,
        MusicSourceMode::YouTube => MusicSource::YouTube,
        MusicSourceMode::Auto => to_source(browser),
    }
}

/// WinRT DateTime (100 нс с 1601 года) в SystemTime.
fn to_system_time(ticks: i64) -> SystemTime {
    const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
    let unix = ticks - EPOCH_DIFF;
    if unix <= 0 {
        return UNIX_EPOCH;
    }
    UNIX_EPOCH + Duration::from_nanos(unix as u64 * 100)
}

fn ticks(d: i64) -> Duration {
    Duration::from_nanos((d.max(0) as u64) * 100)
}

fn playing(s: &Session) -> bool {
    s.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()).map(|st| st == PlaybackStatus::Playing).unwrap_or(false)
}

struct Candidate {
    session: Session,
    kind: MusicSource,
    browser: bool,
    playing: bool,
    updated: i64,
}

impl MediaSession {
    pub fn new(mode: MusicSourceMode) -> Self {
        Self {
            manager: None,
            session: None,
            session_tokens: Vec::new(),
            watched: Vec::new(),
            browser: BrowserMusicDetector::new(),
            mode,
            source: MusicSource::None,
            cover_key: None,
            catalog_key: None,
            video_title: None,
            video_channel: None,
            cover_is_frame: false,
            catalog_cover: false,
            thumb: None,
            previous_thumb: None,
            thumb_key: None,
            thumb_album: None,
            previous_album: None,
            suspect_cover: None,
            refresh_version: 0,
            accept_any_timeline: true,
            is_playing: false,
            title: None,
            artist: None,
            album: None,
            cover: None,
            duration: Duration::ZERO,
            position: Duration::ZERO,
            position_time: SystemTime::now(),
        }
    }

    pub fn start(&mut self, timers: &mut Timers) {
        let manager = Manager::RequestAsync().and_then(|op| op.get());
        let Ok(manager) = manager else {
            // Медиа-сеансы недоступны (урезанная сборка Windows)
            return;
        };
        let _ = manager.SessionsChanged(&TypedEventHandler::new(|_, _| {
            core::post(Event::SessionsChanged);
            Ok(())
        }));
        self.manager = Some(manager);
        self.attach(timers);
    }

    pub fn set_mode(&mut self, mode: MusicSourceMode, timers: &mut Timers) {
        if self.mode != mode {
            self.mode = mode;
            self.attach(timers);
        }
    }

    pub fn now_playing(&self) -> Option<NowPlaying> {
        if self.session.is_none() {
            return None;
        }
        let title = self.title.clone()?;
        Some(NowPlaying {
            title,
            artist: self.artist.clone(),
            album: self.album.clone(),
            cover: self.cover.clone(),
            is_playing: self.is_playing,
            position: self.position,
            position_time: self.position_time,
            duration: self.duration,
            source: self.source,
            video_title: if self.source == MusicSource::YouTube { self.video_title.clone() } else { None },
            cover_is_frame: self.cover_is_frame,
        })
    }

    /// Ролик YouTube как он записан на YouTube и кадр из браузера — чтобы найти сам ролик.
    pub fn video(&self) -> Option<(String, Option<String>, Bytes)> {
        if self.source == MusicSource::YouTube {
            self.video_title.clone().map(|t| (t, self.video_channel.clone(), self.thumb.clone()))
        } else {
            None
        }
    }

    pub fn schedule_attach(&mut self, timers: &mut Timers) {
        timers.set(Timer::Attach, SESSIONS_DELAY);
    }

    /// Выбирает сеанс. true — что-то поменялось и стоит перечитать трек.
    pub fn attach(&mut self, timers: &mut Timers) -> bool {
        if self.manager.is_none() {
            return false;
        }
        let (found, kind) = self.pick(timers);
        // Переподписываемся только на новый сеанс: после перезапуска плеера сеанс новый, хоть и с тем же именем
        if same_session(found.as_ref(), self.session.as_ref()) {
            if self.source == kind {
                return false;
            }
            self.source = kind;
            core::post(Event::Refresh);
            return true;
        }
        self.unsubscribe();
        self.session = found;
        self.source = kind;
        if let Some(s) = &self.session {
            if let Ok(t) = s.PlaybackInfoChanged(&TypedEventHandler::new(|_, _| {
                core::post(Event::Refresh);
                Ok(())
            })) {
                self.session_tokens.push((0, t));
            }
            if let Ok(t) = s.MediaPropertiesChanged(&TypedEventHandler::new(|_, _| {
                core::post(Event::Refresh);
                Ok(())
            })) {
                self.session_tokens.push((1, t));
            }
            if let Ok(t) = s.TimelinePropertiesChanged(&TypedEventHandler::new(|_, _| {
                core::post(Event::Timeline);
                Ok(())
            })) {
                self.session_tokens.push((2, t));
            }
        }
        self.accept_any_timeline = true;
        self.cover_key = None;
        core::post(Event::Refresh);
        true
    }

    fn pick(&mut self, timers: &mut Timers) -> (Option<Session>, MusicSource) {
        let Some(manager) = &self.manager else { return (None, MusicSource::None) };
        let Ok(list) = manager.GetSessions() else { return (None, MusicSource::None) };
        let sessions: Vec<Session> = list.into_iter().collect();
        let any_browser = sessions.iter().any(|s| is_browser(&aumid(s)));
        self.update_watch(&sessions, any_browser, timers);
        // Заголовки окон перебираем, только когда в браузере что-то играет: это самая дорогая проверка
        let site = if any_browser && self.mode == MusicSourceMode::Auto { self.browser.check() } else { BrowserMusic::None };

        let mut candidates = Vec::new();
        for session in sessions {
            let id = aumid(&session);
            let kind = classify(&id, self.mode, site);
            if kind == MusicSource::None {
                continue;
            }
            let is_playing = playing(&session);
            let updated = session.GetTimelineProperties().and_then(|t| t.LastUpdatedTime()).map(|d| d.UniversalTime).unwrap_or(0);
            let browser = is_browser(&id);
            // Пока браузерный сеанс считается музыкой, продлеваем память о вкладке
            if browser {
                self.browser.touch(to_browser(kind));
            }
            candidates.push(Candidate { session, kind, browser, playing: is_playing, updated });
        }
        if candidates.is_empty() {
            return (None, MusicSource::None);
        }
        let playing_now: Vec<&Candidate> = candidates.iter().filter(|c| c.playing).collect();
        // Ничего не играет — остаёмся на том, что играло последним
        if playing_now.is_empty() {
            if let Some(last) = candidates.iter().find(|c| same_session(Some(&c.session), self.session.as_ref())) {
                return (Some(last.session.clone()), last.kind);
            }
        }
        // Приостановленную вкладку браузера берём, только если она уже играла у нас
        let mut pool: Vec<&Candidate> = if !playing_now.is_empty() {
            playing_now
        } else {
            candidates.iter().filter(|c| !c.browser || same_session(Some(&c.session), self.session.as_ref())).collect()
        };
        if pool.is_empty() {
            pool = candidates.iter().collect();
        }
        // Обычный ролик YouTube уступает настоящему плееру
        pool.sort_by(|a, b| {
            let ra = if a.kind == MusicSource::YouTube { 0 } else { 1 };
            let rb = if b.kind == MusicSource::YouTube { 0 } else { 1 };
            rb.cmp(&ra).then(b.updated.cmp(&a.updated))
        });
        (Some(pool[0].session.clone()), pool[0].kind)
    }

    /// Смену «играет/пауза» любого сеанса слушаем событиями, а браузер опрашиваем: смену вкладки он не сообщает.
    fn update_watch(&mut self, sessions: &[Session], any_browser: bool, timers: &mut Timers) {
        self.watched.retain(|(w, token)| {
            let keep = sessions.iter().any(|s| same_session(Some(s), Some(w)));
            if !keep {
                let _ = w.RemovePlaybackInfoChanged(*token);
            }
            keep
        });
        for s in sessions {
            if self.watched.iter().any(|(w, _)| same_session(Some(s), Some(w))) {
                continue;
            }
            if let Ok(token) = s.PlaybackInfoChanged(&TypedEventHandler::new(|_, _| {
                core::post(Event::SessionsChanged);
                Ok(())
            })) {
                self.watched.push((s.clone(), token));
            }
        }
        let poll = any_browser && self.mode == MusicSourceMode::Auto;
        if poll {
            if !timers.is_set(Timer::BrowserPoll) {
                timers.set(Timer::BrowserPoll, BROWSER_TICK);
            }
        } else {
            timers.cancel(Timer::BrowserPoll);
        }
    }

    fn unsubscribe(&mut self) {
        if let Some(s) = &self.session {
            for (kind, token) in self.session_tokens.drain(..) {
                let _ = match kind {
                    0 => s.RemovePlaybackInfoChanged(token),
                    1 => s.RemoveMediaPropertiesChanged(token),
                    _ => s.RemoveTimelinePropertiesChanged(token),
                };
            }
        }
        self.session_tokens.clear();
    }

    /// Новая позиция от плеера. true — позиция действительно сменилась.
    pub fn read_timeline(&mut self) -> bool {
        let Some(session) = self.session.clone() else { return false };
        let Ok(t) = session.GetTimelineProperties() else { return false };
        let start = t.StartTime().map(|d| d.Duration).unwrap_or(0);
        let end = t.EndTime().map(|d| d.Duration).unwrap_or(0);
        let pos = t.Position().map(|d| d.Duration).unwrap_or(0);
        let mut at = t.LastUpdatedTime().map(|d| to_system_time(d.UniversalTime)).unwrap_or_else(|_| SystemTime::now());
        let now = SystemTime::now();
        if at > now || at < UNIX_EPOCH + Duration::from_secs(946_684_800) {
            at = now;
        }
        // Сведения старше нашей оценки (снятой при паузе) только сбили бы позицию назад
        if at < self.position_time && !self.accept_any_timeline {
            return false;
        }
        self.accept_any_timeline = false;
        self.duration = ticks(end - start);
        self.position = ticks(pos - start);
        self.position_time = at;
        true
    }

    /// Перечитывает трек. Возвращает true, если стоит обновить обои.
    pub fn refresh(&mut self, timers: &mut Timers) -> bool {
        let session = self.session.clone();
        self.refresh_version += 1;
        let version = self.refresh_version;
        let mut is_playing = false;
        let (mut title, mut artist, mut album): (Option<String>, Option<String>, Option<String>) = (None, None, None);
        let mut thumbnail: Option<IRandomAccessStreamReference> = None;
        if let Some(s) = &session {
            is_playing = playing(s);
            if let Ok(props) = s.TryGetMediaPropertiesAsync().and_then(|op| op.get()) {
                let nonempty = |h: windows::core::Result<windows::core::HSTRING>| {
                    h.ok().map(|h| h.to_string()).filter(|s| !s.trim().is_empty())
                };
                title = nonempty(props.Title());
                artist = nonempty(props.Artist());
                album = nonempty(props.AlbumTitle());
                thumbnail = props.Thumbnail().ok();
            }
        }

        // Сайт в медиа-сеансе не виден. Если название песни есть в заголовке окна, значит вкладка на виду
        let mut source_changed = false;
        let browser = session.as_ref().map(|s| is_browser(&aumid(s))).unwrap_or(false);
        if browser && self.mode == MusicSourceMode::Auto {
            let matched = self.browser.match_title(title.as_deref());
            if matched != BrowserMusic::None {
                let refined = to_source(matched);
                if refined != self.source {
                    self.source = refined;
                    source_changed = true;
                }
            }
        }

        self.video_title = title.as_deref().map(text::without_site_name);
        self.video_channel = artist.clone();
        if let Some(t) = title.clone() {
            let video = if self.source == MusicSource::YouTube { Some(text::video_title(&t)) } else { None };
            let (ct, ca, cal) = text::clean_metadata(&t, artist.as_deref(), album.as_deref(), self.source);
            title = Some(ct);
            artist = ca;
            album = cal;
            // Опенинг аниме: чего нет в названии, узнаём у AnimeThemes — не ждём, обновимся с ответом
            if let Some(video) = video {
                match anime::try_get_resolved(&video) {
                    Some(Some((at, aa))) => {
                        title = Some(at);
                        artist = aa;
                    }
                    Some(None) => {}
                    None => {
                        crate::media::runtime().spawn(async move {
                            if anime::resolve(video).await.is_some() {
                                core::post(Event::Refresh);
                            }
                        });
                    }
                }
            }
        }

        let key = format!("{}\n{}\n{}", artist.as_deref().unwrap_or(""), title.as_deref().unwrap_or(""), album.as_deref().unwrap_or(""));
        let new_track = Some(&key) != self.cover_key.as_ref();
        if Some(&key) != self.thumb_key.as_ref() {
            self.thumb_key = Some(key.clone());
            self.previous_thumb = self.thumb.take();
            self.previous_album = self.thumb_album.take();
            self.thumb_album = album.clone();
            self.suspect_cover = None;
        }
        // Браузер меняет картинку позже названия — у браузера перечитываем её при каждом событии
        let mut raw: Bytes = None;
        let mut thumb_changed = false;
        if browser || new_track || source_changed || (self.cover.is_none() && thumbnail.is_some()) {
            raw = thumbnail.as_ref().and_then(read_stream).map(Arc::new);
            thumb_changed = !same_bytes(raw.as_deref(), self.thumb.as_deref());
            self.thumb = raw.clone();
        }
        if self.source == MusicSource::YouTube {
            // Браузер присылает логотип сайта или кадр ролика — у YouTube обложка только из каталога
            if new_track {
                self.cover_key = Some(key.clone());
                self.cover = None;
                self.cover_is_frame = false;
                self.catalog_cover = false;
            }
        } else if new_track || source_changed || thumb_changed {
            let mut cover = raw.clone();
            // Если сайт не дал обложку, браузер подставляет свой значок
            if cover.as_deref().map(|c| browser && is_app_icon(c)).unwrap_or(false) {
                cover = None;
            }
            // Картинка прошлого трека с другого альбома — браузер ещё не успел сменить её
            if let Some(c) = &cover {
                if browser && new_track && self.previous_album != album && same_bytes(Some(c), self.previous_thumb.as_deref()) {
                    self.suspect_cover = Some(c.clone());
                    cover = None;
                }
            }
            self.cover_key = Some(key.clone());
            if new_track || source_changed {
                self.cover = cover;
                self.cover_is_frame = false;
                self.catalog_cover = false;
            } else if let Some(c) = cover {
                // Плеер прислал обложку позже названия. Чёткую из каталога мелкой от браузера не заменяем
                if !(self.catalog_cover && pixel_size(&c) < SHARP_COVER) {
                    self.cover = Some(c);
                    self.cover_is_frame = false;
                    self.catalog_cover = false;
                    self.catalog_key = None;
                }
            }
        }

        if is_playing != self.is_playing {
            // Плеер не всегда сообщает позицию при паузе и продолжении — замираем и продолжаем от своей оценки
            let now = SystemTime::now();
            self.position = super::extrapolate(self.position, self.position_time, self.is_playing, self.duration, now);
            self.position_time = now;
            self.is_playing = is_playing;
        }
        if new_track {
            self.position = Duration::ZERO;
            self.position_time = SystemTime::now();
            self.accept_any_timeline = true;
        }
        self.title = title.clone();
        self.artist = artist.clone();
        self.album = album;
        if session.is_some() {
            self.read_timeline();
        }

        // YouTube отдаёт фавикон, а нужна обложка трека — ищем её в каталогах. Так же с другими сайтами в браузере,
        // если пришёл значок браузера или мелкая обложка. Родную обложку десктопных плееров не подменяем.
        let small = self.cover.as_deref().map(|c| browser && pixel_size(c) < SHARP_COVER).unwrap_or(false);
        if let Some(t) = title {
            if (new_track || source_changed || self.catalog_key.as_ref() != Some(&key))
                && (self.source == MusicSource::YouTube || self.cover.is_none() || small)
            {
                timers.cancel(Timer::CoverRetry);
                let variants: Vec<_> = text::candidates(&t, artist.as_deref()).into_iter().take(4).collect();
                let youtube = self.source == MusicSource::YouTube;
                let video = self.video();
                let key = key.clone();
                crate::media::runtime().spawn(async move {
                    let (mut art, mut failed) = cover::look_up(&variants).await;
                    let mut frame = false;
                    // Не нашлось по названию и это YouTube — заглядываем на YouTube: блок «Музыка» или стоп-кадр
                    if art.is_none() && youtube {
                        if let Some((name, channel, thumb)) = &video {
                            if let Some(song) = youtube::find_song(name, channel.as_deref(), thumb.clone()).await {
                                let (a, f) = cover::look_up(&[(song.title.clone(), song.artist.clone())]).await;
                                art = a;
                                failed |= f;
                            }
                            if art.is_none() && !failed {
                                art = youtube::find_cover(name, channel.as_deref(), thumb.clone()).await;
                                frame = art.is_some();
                            }
                        }
                    }
                    core::post(Event::CoverFound { version, key, art, failed, frame });
                });
            }
        }
        true
    }

    /// Ответ каталогов обложек. true — обложка поменялась.
    pub fn cover_found(&mut self, version: u64, key: String, art: Bytes, failed: bool, frame: bool, timers: &mut Timers) -> bool {
        if version != self.refresh_version {
            return false;
        }
        // Каталоги не ответили — трек не считаем проверенным
        if failed {
            timers.set(Timer::CoverRetry, COVER_RETRY);
        } else {
            self.catalog_key = Some(key);
        }
        if art.is_some() || self.source == MusicSource::YouTube {
            self.catalog_cover = art.is_some() && !frame;
            self.cover = art;
            self.cover_is_frame = frame;
            return true;
        }
        if self.cover.is_none() && self.suspect_cover.is_some() {
            // В каталогах нет, а браузер так и не сменил картинку — значит, она всё-таки этого трека
            self.cover = self.suspect_cover.clone();
            self.cover_is_frame = false;
            return true;
        }
        false
    }

    pub fn duration(&self) -> Duration {
        self.duration
    }

    pub fn toggle_play_pause(&self) {
        if let Some(s) = &self.session {
            let _ = s.TryTogglePlayPauseAsync();
        }
    }

    /// Следующий трек. У обычного ролика YouTube «вперёд» нет — доматываем ролик до конца, и YouTube включит следующий.
    pub fn next(&self) {
        let Some(s) = &self.session else { return };
        let Ok(info) = s.GetPlaybackInfo() else { return };
        let controls = info.Controls().ok();
        let next_enabled = controls.as_ref().and_then(|c| c.IsNextEnabled().ok()).unwrap_or(true);
        let pos_enabled = controls.as_ref().and_then(|c| c.IsPlaybackPositionEnabled().ok()).unwrap_or(false);
        if next_enabled || !pos_enabled {
            let _ = s.TrySkipNextAsync();
            return;
        }
        if let Ok(t) = s.GetTimelineProperties() {
            let start = t.StartTime().map(|d| d.Duration).unwrap_or(0);
            let end = t.EndTime().map(|d| d.Duration).unwrap_or(0);
            if end <= start {
                return;
            }
            let _ = s.TryChangePlaybackPositionAsync(end - 3_000_000);
            if !playing(s) {
                let _ = s.TryPlayAsync();
            }
        }
    }

    /// Предыдущий трек; где «назад» нет (обычный ролик YouTube) — к началу трека.
    pub fn previous(&self) {
        let Some(s) = &self.session else { return };
        let Ok(info) = s.GetPlaybackInfo() else { return };
        let controls = info.Controls().ok();
        let prev_enabled = controls.as_ref().and_then(|c| c.IsPreviousEnabled().ok()).unwrap_or(true);
        let pos_enabled = controls.as_ref().and_then(|c| c.IsPlaybackPositionEnabled().ok()).unwrap_or(false);
        if prev_enabled || !pos_enabled {
            let _ = s.TrySkipPreviousAsync();
            return;
        }
        let start = s.GetTimelineProperties().and_then(|t| t.StartTime()).map(|d| d.Duration).unwrap_or(0);
        let _ = s.TryChangePlaybackPositionAsync(start);
    }

    /// Перемотка. Позицию показываем сразу, не дожидаясь, пока плеер подтвердит.
    pub fn seek(&mut self, position: Duration) {
        let Some(s) = self.session.clone() else { return };
        if self.duration.is_zero() {
            return;
        }
        let position = position.min(self.duration);
        self.position = position;
        self.position_time = SystemTime::now();
        self.accept_any_timeline = true;
        let start = s.GetTimelineProperties().and_then(|t| t.StartTime()).map(|d| d.Duration).unwrap_or(0);
        let _ = s.TryChangePlaybackPositionAsync(start + (position.as_nanos() / 100) as i64);
    }

    pub fn shutdown(&mut self) {
        self.unsubscribe();
        for (w, token) in self.watched.drain(..) {
            let _ = w.RemovePlaybackInfoChanged(token);
        }
        self.session = None;
    }
}

pub fn read_stream(reference: &IRandomAccessStreamReference) -> Option<Vec<u8>> {
    let stream = reference.OpenReadAsync().ok()?.get().ok()?;
    let size = stream.Size().ok()?;
    if size == 0 || size > 16 * 1024 * 1024 {
        return None;
    }
    let reader = DataReader::CreateDataReader(&stream).ok()?;
    let op: IAsyncOperation<u32> = reader.LoadAsync(size as u32).ok()?.cast().ok()?;
    op.get().ok()?;
    let mut bytes = vec![0u8; size as usize];
    reader.ReadBytes(&mut bytes).ok()?;
    Some(bytes)
}

fn same_bytes(a: Option<&Vec<u8>>, b: Option<&Vec<u8>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Значок приложения (Chrome, Edge) — картинка с полностью прозрачными углами. Обложки непрозрачны.
pub fn is_app_icon(data: &[u8]) -> bool {
    let Ok(img) = image::load_from_memory(data) else { return false };
    if !img.color().has_alpha() || img.width() < 2 || img.height() < 2 {
        return false;
    }
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width() - 1, rgba.height() - 1);
    [(0, 0), (w, 0), (0, h), (w, h)].iter().all(|&(x, y)| rgba.get_pixel(x, y)[3] < 16)
}

/// Меньшая сторона картинки в пикселях; 0, если её не прочитать.
pub fn pixel_size(data: &[u8]) -> u32 {
    crate::gfx::image_size(data).map(|(w, h)| w.min(h)).unwrap_or(0)
}
