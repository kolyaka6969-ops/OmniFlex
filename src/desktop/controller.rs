//! Когда и что показывать на обоях. Живёт в потоке ядра рядом с медиа-сеансами; окна получают команды
//! в своём потоке. Ролики YouTube попадают на обои, только если это песня (нашёлся текст, обложка в каталоге
//! или по названию видно, что это клип).

use std::time::{Duration, SystemTime};

use crate::core::{self, Event, Timer, Timers};
use crate::media::lyrics::{self, Lyrics, Status};
use crate::media::session::MediaSession;
use crate::media::{cover, text, youtube, MusicSource, NowPlaying};
use crate::settings::Settings;

/// Плеер присылает название раньше длительности, а без неё текст ищется хуже.
const FETCH_DELAY: Duration = Duration::from_millis(250);
const RETRY_DELAY: Duration = Duration::from_secs(5);
/// Сколько держать заглушку, пока новый ролик YouTube проверяется.
const HOLD_LIMIT: Duration = Duration::from_secs(8);

pub enum FetchResult {
    Normal { key: String, lyrics: Lyrics },
    YouTube { track: NowPlaying, outcome: YouTubeOutcome },
}

pub enum YouTubeOutcome {
    /// Песня: название для обоев и текст.
    Music { name: (String, Option<String>), lyrics: Lyrics },
    /// Текста нет. song — песня из описания ролика, art — нашлась обложка в каталоге.
    NoLyrics { failed: bool, song: Option<(String, Option<String>)>, art: bool },
}

pub struct DesktopController {
    enabled: bool,
    lyrics_key: Option<String>,
    track_key: Option<String>,
    music: Option<String>,
    music_name: Option<(String, Option<String>)>,
    pending: Option<String>,
    showing_youtube: bool,
    visible: bool,
    hold_expired: bool,
    shown_track: Option<NowPlaying>,
    generation: u64,
    task: Option<tokio::task::JoinHandle<()>>,
    settings: (String, i32, i32, bool),
}

fn is_music(l: &Lyrics) -> bool {
    matches!(l.status, Status::Synced | Status::Plain | Status::Instrumental)
}

impl DesktopController {
    pub fn new(s: &Settings) -> Self {
        let c = Self {
            enabled: s.desktop_lyrics,
            lyrics_key: None,
            track_key: None,
            music: None,
            music_name: None,
            pending: None,
            showing_youtube: false,
            visible: false,
            hold_expired: false,
            shown_track: None,
            generation: 0,
            task: None,
            settings: (s.desktop_screen.clone(), s.marquee_speed, s.marquee_fps, s.desktop_controls),
        };
        c
    }

    pub fn apply_settings(&mut self, s: &Settings, media: &MediaSession, timers: &mut Timers) {
        let view = (s.desktop_screen.clone(), s.marquee_speed, s.marquee_fps, s.desktop_controls);
        if view != self.settings {
            self.settings = view.clone();
            super::apply_settings(view.0, view.1, view.2, view.3);
        }
        if s.desktop_lyrics != self.enabled {
            self.enabled = s.desktop_lyrics;
            self.sync(media, timers);
        }
    }

    /// Для подтверждённой песни из YouTube — разобранные название и исполнитель, а не заголовок ролика.
    fn shown(&self, track: &NowPlaying) -> NowPlaying {
        match (&self.music, &self.music_name) {
            (Some(m), Some((t, a))) if *m == track.key() => {
                NowPlaying { title: t.clone(), artist: a.clone(), ..track.clone() }
            }
            _ => track.clone(),
        }
    }

    fn show(&mut self, track: NowPlaying) {
        self.shown_track = Some(track.clone());
        super::show_track(track);
    }

    pub fn sync(&mut self, media: &MediaSession, timers: &mut Timers) {
        let track = if self.enabled { media.now_playing() } else { None };
        let key = track.as_ref().map(|t| t.key());
        if key != self.track_key {
            self.track_key = key.clone();
            self.pending = match &track {
                Some(t) if t.source == MusicSource::YouTube && self.showing_youtube => Some(t.key()),
                _ => None,
            };
            if track.is_none() || key != self.music {
                self.music_name = None;
            }
        }
        // Обычное видео YouTube не должно висеть на обоях, пока текст не подтвердил, что это песня.
        // Обложка из каталога значит, что это песня; кадр ролика такого не значит.
        let allowed = match &track {
            Some(t) => {
                t.source != MusicSource::YouTube
                    || Some(t.key()) == self.music
                    || Some(t.key()) == self.pending
                    || (t.cover.is_some() && !t.cover_is_frame)
            }
            None => false,
        };
        self.showing_youtube = allowed && track.as_ref().map(|t| t.source == MusicSource::YouTube).unwrap_or(false);
        if allowed {
            let shown = self.shown(track.as_ref().unwrap());
            timers.cancel(Timer::Hold);
            self.hold_expired = false;
            self.visible = true;
            self.show(shown);
        } else if self.enabled
            && self.visible
            && !self.hold_expired
            && track.as_ref().map(|t| t.source == MusicSource::YouTube).unwrap_or(true)
        {
            // Следующий трек ещё проверяется — вместо рабочего стола заглушка
            if !timers.is_set(Timer::Hold) {
                timers.set(Timer::Hold, HOLD_LIMIT);
                self.shown_track = None;
                super::show_placeholder();
            }
        } else {
            self.hide_now(timers);
        }

        // Длительность входит в ключ: по ней базы отличают версии песни
        let lyrics_key = track.as_ref().map(|t| format!("{}\n{}", t.key(), t.duration.as_secs()));
        if lyrics_key == self.lyrics_key {
            return;
        }
        self.lyrics_key = lyrics_key;
        self.cancel_fetch();
        timers.cancel(Timer::Fetch);
        if let Some(t) = &track {
            timers.set(Timer::Fetch, if t.duration > Duration::ZERO { Duration::from_millis(50) } else { FETCH_DELAY });
        }
    }

    pub fn timeline_changed(&mut self, media: &MediaSession, timers: &mut Timers) {
        let track = if self.enabled { media.now_playing() } else { None };
        let (Some(track), Some(shown)) = (track, &self.shown_track) else {
            self.sync(media, timers);
            return;
        };
        if track.key() != shown.key() {
            self.sync(media, timers);
            return;
        }
        let now = SystemTime::now();
        let play_changed = track.is_playing != shown.is_playing;
        let expected = shown.position_at(now).as_secs_f64();
        let actual = track.position_at(now).as_secs_f64();
        if play_changed || (actual - expected).abs() > 2.0 {
            self.sync(media, timers);
        }
    }

    pub fn hold_expired(&mut self, media: &MediaSession, timers: &mut Timers) {
        // Трек так и не подтвердился — значит, это не песня: убираем заглушку
        self.hold_expired = true;
        self.sync(media, timers);
    }

    fn hide_now(&mut self, timers: &mut Timers) {
        timers.cancel(Timer::Hold);
        self.hold_expired = false;
        if self.visible || self.shown_track.is_some() {
            super::hide();
        }
        self.visible = false;
        self.shown_track = None;
    }

    fn cancel_fetch(&mut self) {
        self.generation += 1;
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }

    pub fn fetch(&mut self, media: &MediaSession) {
        let Some(track) = (if self.enabled { media.now_playing() } else { None }) else { return };
        self.cancel_fetch();
        let generation = self.generation;
        let video = media.video();
        let handle = crate::media::runtime().spawn(async move {
            let result = if track.source == MusicSource::YouTube {
                FetchResult::YouTube { outcome: fetch_youtube(&track, video).await, track }
            } else {
                let lyrics = lyrics::find(track.title.clone(), track.artist.clone(), track.album.clone(), track.duration).await;
                FetchResult::Normal { key: track.key(), lyrics }
            };
            core::post(Event::LyricsFound { generation, result });
        });
        self.task = Some(handle);
    }

    pub fn fetched(&mut self, generation: u64, result: FetchResult, media: &MediaSession, timers: &mut Timers) {
        if generation != self.generation {
            return;
        }
        self.task = None;
        match result {
            FetchResult::Normal { key, lyrics } => {
                let failed = lyrics.status == Status::Error;
                super::show_lyrics(key, lyrics);
                if failed {
                    timers.set(Timer::Fetch, RETRY_DELAY);
                }
            }
            FetchResult::YouTube { track, outcome } => match outcome {
                YouTubeOutcome::Music { name, lyrics } => self.confirm(&track, Some(name), lyrics, media),
                YouTubeOutcome::NoLyrics { failed, song, art } => {
                    if let Some((t, a)) = song {
                        let lyrics = if failed { Lyrics::error() } else { Lyrics::not_found() };
                        self.confirm(&track, Some((t, a.or_else(|| track.artist.clone()))), lyrics, media);
                    } else if art || (!failed && text::is_music_video(track.video_title.as_deref().unwrap_or(&track.title))) {
                        // Неофициальный клип или кавер: ни текста, ни обложки, но по названию видно, что это музыка
                        self.confirm(&track, None, Lyrics::not_found(), media);
                    } else if !(failed && Some(track.key()) == self.pending) {
                        // Базы не ответили, а ролик пришёл следом за песней — оставляем до повторной проверки
                        self.pending = None;
                        self.showing_youtube = false;
                        self.hide_now(timers);
                    }
                    if failed {
                        timers.set(Timer::Fetch, RETRY_DELAY);
                    }
                }
            },
        }
    }

    /// Ролик YouTube оказался песней: показываем его на обоях.
    fn confirm(&mut self, track: &NowPlaying, name: Option<(String, Option<String>)>, lyrics: Lyrics, media: &MediaSession) {
        self.music = Some(track.key());
        self.music_name = name;
        self.pending = None;
        self.showing_youtube = true;
        self.hold_expired = false;
        self.visible = true;
        // Пока искали текст, плеер мог прислать обложку из каталога и новую позицию — берём свежие
        let current = media.now_playing().filter(|n| n.key() == track.key()).unwrap_or_else(|| track.clone());
        let shown = self.shown(&current);
        let key = shown.key();
        self.show(shown);
        super::show_lyrics(key, lyrics);
    }

    pub fn shutdown(&mut self) {
        self.cancel_fetch();
        super::shutdown_blocking();
    }
}

/// Из YouTube приходят и клипы, и лекции, и влоги — на обои пускаем только то, что нашлось в базе текстов.
async fn fetch_youtube(track: &NowPlaying, video: Option<(String, Option<String>, Option<std::sync::Arc<Vec<u8>>>)>) -> YouTubeOutcome {
    let mut candidates: Vec<(String, Option<String>)> = text::candidates(&track.title, track.artist.as_deref()).into_iter().take(4).collect();
    let (mut result, mut index) = lyrics::find_video(candidates.clone(), track.duration).await;
    let mut failed = result.status == Status::Error;

    let mut song: Option<(String, Option<String>)> = None;
    // Текст по названию не нашёлся — возможно, песня указана в описании ролика («Музыка»)
    if !is_music(&result) {
        if let Some((title, channel, thumb)) = video {
            let found = tokio::time::timeout(Duration::from_secs(3), youtube::find_song(&title, channel.as_deref(), thumb)).await.ok().flatten();
            if let Some(s) = found {
                let song_candidates = vec![(s.title.clone(), s.artist.clone())];
                let (song_result, _) = lyrics::find_video(song_candidates.clone(), track.duration).await;
                if is_music(&song_result) {
                    result = song_result.clone();
                    index = 0;
                    candidates = song_candidates;
                }
                failed |= song_result.status == Status::Error;
                song = Some((s.title, s.artist));
            }
        }
    }

    if !is_music(&result) {
        // Текст мог не найтись из-за недоступной базы. Тогда пускаем песню, если её знает каталог обложек
        let art = if track.cover.is_some() && !track.cover_is_frame { true } else { cover::find(&candidates).await.is_some() };
        return YouTubeOutcome::NoLyrics { failed, song, art };
    }
    let (t, a) = candidates[index].clone();
    YouTubeOutcome::Music { name: (t, a.or_else(|| track.artist.clone())), lyrics: result }
}
