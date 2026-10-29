//! Текст песни на рабочем столе вместо обоев. Окна работают в своём потоке: Explorer связывает очередь ввода
//! дочернего окна со своей, и подвисание другого потока OmniFlex подвесило бы рабочий стол.

pub mod controller;
pub mod layer;
pub mod pointer;
pub mod view;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer, WM_TIMER};

use crate::core::{self, Event};
use crate::media::lyrics::Lyrics;
use crate::media::NowPlaying;
use crate::settings::{self, ALL_SCREENS, PRIMARY_SCREEN};
use crate::wnd::{self, Dispatcher, Handler};
use crate::win;
use layer::Layer;
use pointer::{Control, Target};
use view::View;

static DESKTOP: Dispatcher = Dispatcher::new();

const TIMER_WATCH: usize = 1;
const TIMER_FRAME: usize = 2;

struct Screen {
    view: Rc<RefCell<View>>,
    layer: Layer,
}

#[derive(Default)]
struct State {
    hwnd: isize,
    screens: Vec<Screen>,
    shown_track: Option<NowPlaying>,
    shown_lyrics: Option<(String, Lyrics)>,
    shown: bool,
    setting: String,
    monitors: Vec<win::Monitor>,
    watch_ticks: u32,
    frame_due: Option<Instant>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn post(task: impl FnOnce(&mut State) + Send + 'static) {
    ensure_thread();
    DESKTOP.post(move || {
        STATE.with(|s| {
            if let Ok(mut s) = s.try_borrow_mut() {
                task(&mut s);
            }
        });
        schedule_frame();
    });
}

fn ensure_thread() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("OmniFlex desktop".into())
            .spawn(move || {
                // Explorer может работать с другим режимом DPI — без этого Windows не даст вставить окно
                unsafe {
                    let _ = windows::Win32::UI::HiDpi::SetThreadDpiHostingBehavior(windows::Win32::UI::HiDpi::DPI_HOSTING_BEHAVIOR_MIXED);
                }
                let hwnd = wnd::create_message_window("OmniFlexDesktopTimers", Rc::new(RefCell::new(TimerWindow)));
                DESKTOP.attach("OmniFlexDesktopQueue");
                let screen = settings::read(|s| s.desktop_screen.clone());
                STATE.with(|s| {
                    let mut s = s.borrow_mut();
                    s.hwnd = hwnd.map(|h| h.0 as isize).unwrap_or(0);
                    s.setting = screen.clone();
                    rebuild(&mut s);
                });
                if let Some(h) = hwnd {
                    unsafe {
                        SetTimer(h, TIMER_WATCH, 1000, None);
                    }
                }
                let _ = tx.send(());
                wnd::run_message_loop();
            })
            .expect("desktop thread");
        let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
    });
}

struct TimerWindow;

impl Handler for TimerWindow {
    fn handle(&mut self, _hwnd: HWND, msg: u32, wparam: WPARAM, _l: LPARAM) -> Option<LRESULT> {
        if msg != WM_TIMER {
            return None;
        }
        STATE.with(|s| {
            let Ok(mut s) = s.try_borrow_mut() else { return };
            match wparam.0 {
                TIMER_WATCH => watch(&mut s),
                TIMER_FRAME => {
                    s.frame_due = None;
                    unsafe {
                        let _ = KillTimer(HWND(s.hwnd as _), TIMER_FRAME);
                    }
                }
                _ => {}
            }
        });
        schedule_frame();
        Some(LRESULT(0))
    }
}

/// Мониторы, на которых должны быть обои: имена устройств.
fn devices(setting: &str) -> Vec<String> {
    let all = win::monitors();
    if setting == ALL_SCREENS && !all.is_empty() {
        return all.into_iter().map(|m| m.device).collect();
    }
    if setting != PRIMARY_SCREEN && all.iter().any(|m| m.device == setting) {
        return vec![setting.to_string()];
    }
    // Выбранный монитор отключён — показываем на основном
    vec![win::primary_monitor().device]
}

/// Своё окно на каждый нужный монитор: макет у каждого подгоняется под его экран.
fn rebuild(s: &mut State) {
    let wanted = devices(&s.setting);
    s.screens.retain(|scr| wanted.contains(&scr.layer.device().to_string()));
    let settings = settings::get();
    for device in wanted {
        if s.screens.iter().any(|scr| scr.layer.device() == device) {
            continue;
        }
        let m = win::monitor_by_device(&device);
        let view = View::new((m.bounds.right - m.bounds.left) as u32, (m.bounds.bottom - m.bounds.top) as u32);
        let view = Rc::new(RefCell::new(view));
        {
            let mut v = view.borrow_mut();
            v.set_marquee(settings.marquee_speed, settings.marquee_fps);
            v.set_controls(settings.desktop_controls);
        }
        let mut screen = Screen { layer: Layer::new(device, view.clone()), view };
        // Новый монитор подхватывает то, что уже показано на остальных
        if s.shown {
            if let Some(track) = &s.shown_track {
                let mut v = screen.view.borrow_mut();
                v.show_track(track.clone());
                if let Some((key, lyrics)) = &s.shown_lyrics {
                    v.show_lyrics(key, lyrics.clone());
                }
            }
            screen.layer.set_visible(true);
        }
        s.screens.push(screen);
    }
    s.monitors = win::monitors();
    update_pointer(s);
}

fn watch(s: &mut State) {
    s.watch_ticks = s.watch_ticks.wrapping_add(1);
    // Подключили или сняли монитор, сменили разрешение
    if s.watch_ticks % 3 == 0 {
        let monitors = win::monitors();
        if monitors != s.monitors {
            for scr in &mut s.screens {
                let m = win::monitor_by_device(scr.layer.device());
                scr.view.borrow_mut().resize((m.bounds.right - m.bounds.left) as u32, (m.bounds.bottom - m.bounds.top) as u32);
            }
            rebuild(s);
        }
    }
    for scr in &mut s.screens {
        scr.layer.ensure();
    }
    if s.watch_ticks % 5 == 0 {
        update_targets(s);
    }
}

/// Мышь слушаем, только пока обои с кнопками на экране.
fn update_pointer(s: &mut State) {
    let on = s.shown && settings::read(|st| st.desktop_controls);
    if on {
        update_targets(s);
    } else {
        pointer::set_targets(Vec::new());
    }
    pointer::set_enabled(on);
}

fn update_targets(s: &State) {
    let mut targets = Vec::new();
    for (i, scr) in s.screens.iter().enumerate() {
        if !scr.layer.visible() {
            continue;
        }
        let origin = win::monitor_by_device(scr.layer.device()).bounds;
        for (control, b) in scr.view.borrow().control_bounds() {
            targets.push(Target {
                bounds: RECT {
                    left: origin.left + b[0].round() as i32,
                    top: origin.top + b[1].round() as i32,
                    right: origin.left + b[2].round() as i32,
                    bottom: origin.top + b[3].round() as i32,
                },
                owner: i,
                control,
            });
        }
    }
    pointer::set_targets(targets);
}

/// Пересчитать и вывести кадр, затем завести таймер на следующий.
fn schedule_frame() {
    STATE.with(|s| {
        let Ok(mut s) = s.try_borrow_mut() else { return };
        if !s.shown {
            return;
        }
        let mut next: Option<Instant> = None;
        // Частота кадров: анимации — 60 в секунду, одна бегущая строка — сколько задано в настройках
        let mut interval: Option<u64> = None;
        for scr in &s.screens {
            if !scr.layer.visible() {
                continue;
            }
            let mut v = scr.view.borrow_mut();
            let wake = v.update();
            let rects = v.render();
            let (anim, marquee) = v.animating();
            if anim {
                interval = Some(interval.map_or(16, |i| i.min(16)));
            }
            if marquee {
                let ms = (1000 / v.marquee_fps().max(1)) as u64;
                interval = Some(interval.map_or(ms, |i| i.min(ms)));
            }
            if let Some(w) = wake {
                next = Some(next.map_or(w, |n| n.min(w)));
            }
            drop(v);
            if let Some(hwnd) = scr.layer.hwnd() {
                for r in rects {
                    let rect = RECT { left: r[0], top: r[1], right: r[2], bottom: r[3] };
                    unsafe {
                        let _ = InvalidateRect(hwnd, Some(&rect), false);
                    }
                }
            }
        }
        let delay = match (interval, next) {
            (Some(i), _) => i,
            (None, Some(n)) => n.saturating_duration_since(Instant::now()).as_millis() as u64,
            (None, None) => return,
        }
        .clamp(4, 60_000);
        s.frame_due = Some(Instant::now() + std::time::Duration::from_millis(delay));
        unsafe {
            SetTimer(HWND(s.hwnd as _), TIMER_FRAME, delay as u32, None);
        }
    });
}

// ---------- Команды из потока ядра ----------

pub fn show_track(track: NowPlaying) {
    post(move |s| {
        if s.shown_track.as_ref().map(|t| t.key()) != Some(track.key()) {
            s.shown_lyrics = None;
        }
        s.shown_track = Some(track.clone());
        s.shown = true;
        for scr in &mut s.screens {
            scr.view.borrow_mut().show_track(track.clone());
            scr.layer.set_visible(true);
        }
        update_pointer(s);
    });
}

/// Заглушка между треками: обои остаются, пока следующий трек проверяется.
pub fn show_placeholder() {
    post(|s| {
        if !s.shown {
            return;
        }
        s.shown_track = None;
        s.shown_lyrics = None;
        for scr in &s.screens {
            scr.view.borrow_mut().show_placeholder();
        }
    });
}

pub fn hide() {
    post(|s| {
        s.shown = false;
        for scr in &mut s.screens {
            scr.view.borrow_mut().stop();
            scr.layer.set_visible(false);
        }
        update_pointer(s);
    });
}

pub fn show_lyrics(key: String, lyrics: Lyrics) {
    post(move |s| {
        if s.shown_track.as_ref().map(|t| t.key()) != Some(key.clone()) {
            return;
        }
        s.shown_lyrics = Some((key.clone(), lyrics.clone()));
        for scr in &s.screens {
            scr.view.borrow_mut().show_lyrics(&key, lyrics.clone());
        }
    });
}

pub fn apply_settings(screen: String, speed: i32, fps: i32, controls: bool) {
    post(move |s| {
        for scr in &s.screens {
            let mut v = scr.view.borrow_mut();
            v.set_marquee(speed, fps);
            v.set_controls(controls);
        }
        if s.setting != screen {
            s.setting = screen;
            rebuild(s);
        }
        update_pointer(s);
    });
}

/// Убрать окна и дождаться: при выходе Explorer должен вернуть обычные обои.
pub fn shutdown_blocking() {
    if !DESKTOP.is_attached() {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    DESKTOP.post(move || {
        pointer::set_enabled(false);
        STATE.with(|s| {
            if let Ok(mut s) = s.try_borrow_mut() {
                s.shown = false;
                s.screens.clear();
            }
        });
        let _ = tx.send(());
    });
    let _ = rx.recv_timeout(std::time::Duration::from_secs(2));
}

// ---------- Мышь (из потока хука) ----------

pub(crate) fn pointer_event(target: Option<Target>, pressed: bool, fraction: f64) {
    DESKTOP.post(move || {
        STATE.with(|s| {
            if let Ok(s) = s.try_borrow() {
                for (i, scr) in s.screens.iter().enumerate() {
                    let control = target.filter(|t| t.owner == i).map(|t| t.control);
                    scr.view.borrow_mut().set_pointer(control, pressed, fraction);
                }
            }
        });
        schedule_frame();
    });
}

pub(crate) fn pointer_click(target: Target, fraction: f64) {
    core::post(match target.control {
        Control::Previous => Event::Previous,
        Control::PlayPause => Event::PlayPause,
        Control::Next => Event::Next,
        Control::Seek => Event::Seek(fraction),
    });
}

