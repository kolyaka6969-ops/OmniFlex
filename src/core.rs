//! Поток ядра: медиа-сеансы Windows и логика текста песни на рабочем столе. Однопоточный цикл событий
//! с таймерами; сеть работает в tokio и возвращает ответы событиями сюда же.

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::desktop::controller::DesktopController;
use crate::media::session::MediaSession;
use crate::settings::{self, Settings};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Timer {
    Attach,
    BrowserPoll,
    CoverRetry,
    Fetch,
    Hold,
}

pub enum Event {
    SessionsChanged,
    Refresh,
    Timeline,
    CoverFound { version: u64, key: String, art: Option<Arc<Vec<u8>>>, failed: bool, frame: bool },
    LyricsFound { generation: u64, result: crate::desktop::controller::FetchResult },
    Settings(Settings),
    Previous,
    PlayPause,
    Next,
    /// Перемотка в долю трека (0..1).
    Seek(f64),
    /// Плеер острова: команда, сменились сеансы, сменился трек, нашёлся текст.
    Player(crate::media::player::Command),
    PlayerChanged,
    PlayerRead,
    PlayerLyrics { key: String, lyrics: crate::media::lyrics::Lyrics },
    Shutdown(Sender<()>),
}

#[derive(Default)]
pub struct Timers {
    list: Vec<(Instant, Timer)>,
}

impl Timers {
    pub fn set(&mut self, timer: Timer, delay: Duration) {
        self.cancel(timer);
        self.list.push((Instant::now() + delay, timer));
    }
    pub fn cancel(&mut self, timer: Timer) {
        self.list.retain(|(_, t)| *t != timer);
    }
    pub fn is_set(&self, timer: Timer) -> bool {
        self.list.iter().any(|(_, t)| *t == timer)
    }
    fn next_deadline(&self) -> Option<Instant> {
        self.list.iter().map(|(i, _)| *i).min()
    }
    fn take_due(&mut self) -> Vec<Timer> {
        let now = Instant::now();
        let due: Vec<Timer> = self.list.iter().filter(|(i, _)| *i <= now).map(|(_, t)| *t).collect();
        self.list.retain(|(i, _)| *i > now);
        due
    }
}

fn sender() -> &'static Mutex<Option<Sender<Event>>> {
    static S: OnceLock<Mutex<Option<Sender<Event>>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(None))
}

pub fn post(event: Event) {
    if let Ok(s) = sender().lock() {
        if let Some(s) = s.as_ref() {
            let _ = s.send(event);
        }
    }
}

pub fn start() {
    let (tx, rx) = channel();
    if let Ok(mut s) = sender().lock() {
        *s = Some(tx);
    }
    std::thread::Builder::new()
        .name("OmniFlex core".into())
        .spawn(move || {
            unsafe {
                let _ = windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED);
            }
            run(rx);
        })
        .expect("core thread");
}

/// Перед выходом: убрать обои с песней (Explorer вернёт обычные) и дождаться этого.
pub fn shutdown_blocking() {
    let (tx, rx) = channel();
    post(Event::Shutdown(tx));
    let _ = rx.recv_timeout(Duration::from_secs(3));
}

struct Core {
    player: crate::media::player::Player,
    media: MediaSession,
    desktop: DesktopController,
    timers: Timers,
}

fn run(rx: Receiver<Event>) {
    let s = settings::get();
    let mut core = Core { player: crate::media::player::Player::new(), media: MediaSession::new(s.source), desktop: DesktopController::new(&s), timers: Timers::default() };
    core.media.start(&mut core.timers);
    core.player.start();
    loop {
        let event = match core.timers.next_deadline() {
            Some(deadline) => {
                let wait = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(wait) {
                    Ok(e) => Some(e),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            None => match rx.recv() {
                Ok(e) => Some(e),
                Err(_) => return,
            },
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(e) = event {
                if core.handle(e) {
                    return true;
                }
            }
            for timer in core.timers.take_due() {
                core.on_timer(timer);
            }
            false
        }));
        match result {
            Ok(true) => return,
            Ok(false) => {}
            Err(_) => crate::util::log_error("panic в потоке ядра"),
        }
    }
}

impl Core {
    /// true — пора выходить.
    fn handle(&mut self, event: Event) -> bool {
        match event {
            Event::SessionsChanged => self.media.schedule_attach(&mut self.timers),
            Event::Refresh => {
                if self.media.refresh(&mut self.timers) {
                    self.desktop.sync(&self.media, &mut self.timers);
                }
            }
            Event::Timeline => {
                if self.media.read_timeline() {
                    self.desktop.timeline_changed(&self.media, &mut self.timers);
                }
            }
            Event::CoverFound { version, key, art, failed, frame } => {
                if self.media.cover_found(version, key, art, failed, frame, &mut self.timers) {
                    self.desktop.sync(&self.media, &mut self.timers);
                }
            }
            Event::LyricsFound { generation, result } => self.desktop.fetched(generation, result, &self.media, &mut self.timers),
            Event::Player(cmd) => self.player.handle(cmd),
            Event::PlayerChanged => self.player.reload(),
            Event::PlayerRead => self.player.read(),
            Event::PlayerLyrics { key, lyrics } => self.player.lyrics_found(key, lyrics),
            Event::Settings(s) => {
                self.media.set_mode(s.source, &mut self.timers);
                self.desktop.apply_settings(&s, &self.media, &mut self.timers);
            }
            Event::Previous => self.media.previous(),
            Event::PlayPause => self.media.toggle_play_pause(),
            Event::Next => self.media.next(),
            Event::Seek(fraction) => {
                let target = self.media.duration().mul_f64(fraction.clamp(0.0, 1.0));
                self.media.seek(target);
                self.desktop.timeline_changed(&self.media, &mut self.timers);
            }
            Event::Shutdown(done) => {
                self.desktop.shutdown();
                self.player.shutdown();
                self.media.shutdown();
                let _ = done.send(());
                return true;
            }
        }
        false
    }

    fn on_timer(&mut self, timer: Timer) {
        match timer {
            Timer::Attach | Timer::BrowserPoll => {
                self.media.attach(&mut self.timers);
            }
            Timer::CoverRetry => {
                if self.media.refresh(&mut self.timers) {
                    self.desktop.sync(&self.media, &mut self.timers);
                }
            }
            Timer::Fetch => self.desktop.fetch(&self.media),
            Timer::Hold => self.desktop.hold_expired(&self.media, &mut self.timers),
        }
    }
}
