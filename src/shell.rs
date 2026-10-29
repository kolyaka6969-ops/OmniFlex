//! Поток окон: значок в трее, горячие клавиши, микшер (Core Audio), остров, индикатор громкости и
//! компактный микшер у трея. Всё живёт в одном потоке со своим циклом сообщений; другие потоки
//! передают сюда задачи через `post`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK,
    RID_INPUT, RIM_TYPEKEYBOARD,
};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, GetSystemMetrics, KillTimer, LoadImageW,
    PostMessageW, RegisterWindowMessageW, SetForegroundWindow, SetTimer, TrackPopupMenu, HICON, IMAGE_ICON,
    LR_DEFAULTCOLOR, MF_CHECKED, MF_SEPARATOR, MF_STRING, SM_CXSMICON, SM_CYSMICON, TPM_BOTTOMALIGN,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_HOTKEY, WM_INPUT, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_TIMER,
};

use crate::audio::{self, Mixer, Target};
use crate::settings::{self, HotkeyAction, HotkeyTarget, Settings};
use crate::wnd::{self, Dispatcher, Handler};
use crate::i18n::tr;
use crate::{flyout, hotkeys, island, osd, process_info, sysinfo, util};

pub static SHELL: Dispatcher = Dispatcher::new();

/// Окно микшера открыто где-то (в настройках или у трея) — индикаторы уровня опрашиваются часто.
pub static MIXER_VIEW_VISIBLE: AtomicBool = AtomicBool::new(false);

const WM_TRAY: u32 = WM_APP + 1;
const TIMER_POLL: usize = 1;
const TIMER_PEAKS: usize = 2;

thread_local! {
    static MIXER: RefCell<Option<Mixer>> = const { RefCell::new(None) };
    static HOTKEYS: RefCell<Option<hotkeys::Registry>> = const { RefCell::new(None) };
    static STATE: RefCell<ShellState> = RefCell::new(ShellState::default());
}

#[derive(Default)]
struct ShellState {
    hwnd: isize,
    tray_added: bool,
    tray_tip: String,
    taskbar_created: u32,
    poll_ticks: u32,
    peak_ticks: u32,
    peaks_on: bool,
    last_version: u64,
}

/// Задача в потоке окон.
pub fn post(task: impl FnOnce() + Send + 'static) {
    SHELL.post(task);
}

/// Микшер потока окон. Вызывать только из этого потока; окон внутри не трогать.
pub fn with_mixer<R>(f: impl FnOnce(&mut Mixer) -> R) -> Option<R> {
    MIXER.with(|m| m.try_borrow_mut().ok().and_then(|mut m| m.as_mut().map(f)))
}

/// Громкость из другого потока (окно настроек).
pub fn set_volume(target: Target, volume: f32) {
    post(move || {
        with_mixer(|m| m.set_volume(&target, volume));
        after_mixer_change();
    });
}

pub fn set_mute(target: Target, muted: bool) {
    post(move || {
        with_mixer(|m| m.set_mute(&target, muted));
        after_mixer_change();
    });
}

pub fn set_device(id: String) {
    post(move || {
        with_mixer(|m| m.set_device(&id));
        after_mixer_change();
    });
}

pub fn refresh_mixer() {
    post(|| {
        with_mixer(|m| {
            m.refresh_devices();
            m.refresh();
        });
        after_mixer_change();
    });
}

pub fn suspend_hotkeys(suspend: bool) {
    post(move || {
        HOTKEYS.with(|h| {
            if let Some(h) = h.borrow_mut().as_mut() {
                h.suspend(suspend);
            }
        });
    });
}

/// Запускает поток окон и ждёт, пока он поднимется.
pub fn start() {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("OmniFlex shell".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            run(tx);
        })
        .expect("shell thread");
    let _ = rx.recv_timeout(std::time::Duration::from_secs(10));
}

fn run(ready: std::sync::mpsc::Sender<()>) {
    MIXER.with(|m| *m.borrow_mut() = Some(Mixer::new()));
    let handler = Rc::new(RefCell::new(ShellWindow));
    let Some(hwnd) = wnd::create_message_window("OmniFlexShell", handler) else {
        util::log_error("не удалось создать окно потока окон");
        let _ = ready.send(());
        return;
    };
    SHELL.attach("OmniFlexShellQueue");
    let taskbar_created = unsafe { RegisterWindowMessageW(PCWSTR(util::wide("TaskbarCreated").as_ptr())) };
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.hwnd = hwnd.0 as isize;
        s.taskbar_created = taskbar_created;
    });

    HOTKEYS.with(|h| *h.borrow_mut() = Some(hotkeys::Registry::new(hwnd)));
    apply_settings(&settings::get());
    add_tray(hwnd);
    register_raw_keyboard(hwnd);
    unsafe {
        SetTimer(hwnd, TIMER_POLL, 500, None);
    }
    let _ = ready.send(());
    wnd::run_message_loop();
}

/// Сырой ввод клавиатуры даже в фоне: по нему остров замечает Caps Lock и смену раскладки сразу.
fn register_raw_keyboard(hwnd: HWND) {
    let device = RAWINPUTDEVICE { usUsagePage: 0x01, usUsage: 0x06, dwFlags: RIDEV_INPUTSINK, hwndTarget: hwnd };
    unsafe {
        let _ = RegisterRawInputDevices(&[device], std::mem::size_of::<RAWINPUTDEVICE>() as u32);
    }
}

fn hwnd() -> HWND {
    HWND(STATE.with(|s| s.borrow().hwnd) as _)
}

/// Настройки поменялись (из окна настроек или меню трея).
pub fn settings_changed(old: Settings, new: Settings) {
    post(move || {
        if old.hotkeys != new.hotkeys {
            HOTKEYS.with(|h| {
                if let Some(h) = h.borrow_mut().as_mut() {
                    h.apply(&new.hotkeys);
                }
            });
        }
        if old.island.output_device != new.island.output_device || old.island.mic_device != new.island.mic_device {
            with_mixer(|m| {
                m.refresh_devices();
                m.refresh();
            });
            after_mixer_change();
        }
        if old.island.player != new.island.player || old.island.lyrics != new.island.lyrics {
            crate::media::player::command(crate::media::player::Command::Reload);
        }
        if old.island.notifications != new.island.notifications {
            crate::notify::apply(new.island.notifications);
        }
        if old.island != new.island
            || old.theme != new.theme
            || old.language != new.language
            || old.launcher != new.launcher
            || old.tools.colors != new.tools.colors
            || old.reminders != new.reminders
        {
            island::apply_settings(&new);
        }
        if old.theme != new.theme {
            flyout::theme_changed();
        }
    });
}

fn apply_settings(s: &Settings) {
    HOTKEYS.with(|h| {
        if let Some(h) = h.borrow_mut().as_mut() {
            h.apply(&s.hotkeys);
        }
    });
    island::apply_settings(s);
    crate::notify::apply(s.island.notifications);
}

/// После изменения громкости: перерисовать окна, которые её показывают.
pub fn after_mixer_change() {
    let snap = audio::snapshot();
    STATE.with(|s| s.borrow_mut().last_version = snap.version);
    flyout::mixer_changed();
    island::mixer_changed();
    update_tray_tip(&snap);
    crate::ui::request_repaint();
}

struct ShellWindow;

impl Handler for ShellWindow {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_HOTKEY => {
                let binding = HOTKEYS.with(|h| h.borrow().as_ref().and_then(|h| h.get(wparam.0 as i32).cloned()));
                if let Some(b) = binding {
                    on_hotkey(&b);
                }
                Some(LRESULT(0))
            }
            WM_TRAY => {
                match lparam.0 as u32 {
                    WM_LBUTTONUP => flyout::toggle(),
                    WM_RBUTTONUP => show_tray_menu(hwnd),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_TIMER => {
                match wparam.0 {
                    TIMER_POLL => on_poll(),
                    TIMER_PEAKS => on_peaks(),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_INPUT => {
                on_raw_input(lparam);
                None
            }
            _ => {
                let created = STATE.with(|s| s.borrow().taskbar_created);
                if created != 0 && msg == created {
                    // Проводник перезапустился — значок в трее пропал, ставим заново
                    STATE.with(|s| s.borrow_mut().tray_added = false);
                    add_tray(hwnd);
                    return Some(LRESULT(0));
                }
                None
            }
        }
    }
}

fn on_raw_input(lparam: LPARAM) {
    unsafe {
        let mut size = 0u32;
        let header = std::mem::size_of::<RAWINPUTHEADER>() as u32;
        GetRawInputData(HRAWINPUT(lparam.0 as _), RID_INPUT, None, &mut size, header);
        if size == 0 || size as usize > 256 {
            return;
        }
        let mut buf = [0u8; 256];
        if GetRawInputData(HRAWINPUT(lparam.0 as _), RID_INPUT, Some(buf.as_mut_ptr() as *mut _), &mut size, header)
            != size
        {
            return;
        }
        let raw = &*(buf.as_ptr() as *const RAWINPUT);
        if raw.header.dwType != RIM_TYPEKEYBOARD.0 {
            return;
        }
        let kb = raw.data.keyboard;
        const RI_KEY_BREAK: u16 = 1;
        // Caps Lock отпущен — переключатель уже сменился. Любая клавиша после Alt+Shift/Win+Space — проверка раскладки
        if kb.Flags & RI_KEY_BREAK != 0 {
            island::key_released(kb.VKey as u32);
        }
    }
}

fn on_poll() {
    let mut st = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.poll_ticks = s.poll_ticks.wrapping_add(1);
        (s.poll_ticks, s.peaks_on)
    });
    let visible = MIXER_VIEW_VISIBLE.load(Ordering::Relaxed) || flyout::is_visible();
    if visible != st.1 {
        STATE.with(|s| s.borrow_mut().peaks_on = visible);
        unsafe {
            if visible {
                with_mixer(|m| m.refresh());
                SetTimer(hwnd(), TIMER_PEAKS, 50, None);
            } else {
                let _ = KillTimer(hwnd(), TIMER_PEAKS);
            }
        }
        st.1 = visible;
    }
    // Раз в 2 секунды — устройства и приложения: подключили наушники, запустили плеер
    if st.0 % 4 == 0 {
        with_mixer(|m| {
            m.refresh_devices();
            if !visible {
                m.refresh();
            }
        });
    } else {
        with_mixer(|m| m.poll_master());
    }
    notify_if_changed();
}

fn on_peaks() {
    let tick = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.peak_ticks = s.peak_ticks.wrapping_add(1);
        s.peak_ticks
    });
    with_mixer(|m| {
        if tick % 20 == 0 {
            m.refresh();
        }
        m.update_peaks();
    });
    notify_if_changed();
}

fn notify_if_changed() {
    let snap = audio::snapshot();
    let changed = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let changed = s.last_version != snap.version;
        s.last_version = snap.version;
        changed
    });
    if changed {
        flyout::mixer_changed();
        island::mixer_changed();
        update_tray_tip(&snap);
        if MIXER_VIEW_VISIBLE.load(Ordering::Relaxed) {
            crate::ui::request_repaint();
        }
    }
}

// ---------- Горячие клавиши ----------

fn on_hotkey(b: &settings::HotkeyBinding) {
    let s = settings::get();
    match b.action {
        HotkeyAction::OpenMixer => {
            flyout::toggle();
            return;
        }
        HotkeyAction::NextDevice => {
            let device = with_mixer(|m| {
                let d = m.next_device();
                m.refresh();
                d
            })
            .flatten();
            after_mixer_change();
            if s.show_osd {
                if let Some(d) = device {
                    osd::show_message(d.glyph(), &d.name);
                }
            }
            return;
        }
        HotkeyAction::ToggleMic => {
            let state = with_mixer(|m| m.toggle_mic()).flatten();
            after_mixer_change();
            match state {
                Some(muted) => {
                    island::mic_toggled(muted);
                    if s.show_osd {
                        osd::show_message(
                            if muted { '\u{F781}' } else { '\u{E720}' },
                            if muted { tr("Микрофон выключен", "Microphone off") } else { tr("Микрофон включён", "Microphone on") },
                        );
                    }
                }
                None => osd::show_message('\u{F781}', tr("Нет микрофона", "No microphone")),
            }
            return;
        }
        HotkeyAction::ToggleIsland => {
            settings::update(|s| s.island.enabled = !s.island.enabled);
            return;
        }
        HotkeyAction::ScreenshotRegion => return crate::tools::screenshot_region(),
        HotkeyAction::ScreenshotFull => return crate::tools::screenshot_full(),
        HotkeyAction::ScreenText => return crate::tools::screen_text(),
        HotkeyAction::QrCode => return crate::tools::qr_code(),
        HotkeyAction::ColorPicker => return crate::tools::color_picker(),
        HotkeyAction::Record => return crate::tools::toggle_record(),
        HotkeyAction::PastePlain => return crate::tools::paste_plain(),
        HotkeyAction::Voice => return crate::voice::toggle(),
        HotkeyAction::AiCommand => return crate::ai::command(),
        HotkeyAction::ClickThrough => {
            settings::update(|s| s.island.click_through = !s.island.click_through);
            let on = settings::read(|s| s.island.click_through);
            osd::show_message('\u{E7C9}', if on { tr("Сквозной режим включён", "Click-through on") } else { tr("Сквозной режим выключен", "Click-through off") });
            return;
        }
        HotkeyAction::Launch => {
            if let Some(id) = &b.app {
                crate::launcher::launch_id(id);
            }
            return;
        }
        _ => {}
    }

    with_mixer(|m| m.refresh());
    let (target, missing) = match b.target {
        HotkeyTarget::Master => (Target::Master, tr("Нет устройства вывода", "No output device").to_string()),
        HotkeyTarget::App => {
            let app = b.app.clone().unwrap_or_default();
            (Target::App(app.clone()), format!("{app}: {}", tr("нет звука", "no sound")))
        }
        HotkeyTarget::Foreground => {
            let pid = crate::win::window_pid(sysinfo::foreground());
            let info = process_info::get(pid);
            let name = info.as_ref().map(|i| i.name.clone()).unwrap_or_else(|| tr("Активное окно", "Active window").into());
            match info {
                Some(i) => (Target::App(i.key), format!("{name}: {}", tr("нет звука", "no sound"))),
                None => (Target::App(String::new()), format!("{name}: {}", tr("нет звука", "no sound"))),
            }
        }
    };

    let Some(channel) = with_mixer(|m| m.channel(&target)).flatten() else {
        if s.show_osd {
            osd::show_message('\u{E7BA}', &missing);
        }
        return;
    };

    let step = s.volume_step as f32;
    with_mixer(|m| match b.action {
        HotkeyAction::VolumeUp => {
            if channel.muted {
                m.set_mute(&target, false);
            }
            m.set_volume(&target, channel.volume + step);
        }
        HotkeyAction::VolumeDown => m.set_volume(&target, channel.volume - step),
        HotkeyAction::ToggleMute => m.set_mute(&target, !channel.muted),
        _ => {}
    });
    after_mixer_change();
    if s.show_osd {
        if let Some(updated) = with_mixer(|m| m.channel(&target)).flatten() {
            osd::show_channel(&updated);
        }
    } else if target == Target::Master {
        // Индикатор выключен — общую громкость показывает остров
        island::volume_changed();
    }
}

// ---------- Трей ----------

fn app_icon(size: i32) -> HICON {
    unsafe {
        let instance = GetModuleHandleW(None).unwrap_or_default();
        LoadImageW(instance, PCWSTR(1 as *const u16), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
            .map(|h| HICON(h.0))
            .unwrap_or_default()
    }
}

fn tray_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        ..Default::default()
    };
    unsafe {
        data.hIcon = app_icon(GetSystemMetrics(SM_CXSMICON).max(GetSystemMetrics(SM_CYSMICON)));
    }
    let tip = STATE.with(|s| s.borrow().tray_tip.clone());
    set_tip(&mut data, if tip.is_empty() { util::APP_NAME } else { &tip });
    data
}

fn set_tip(data: &mut NOTIFYICONDATAW, text: &str) {
    let w: Vec<u16> = text.encode_utf16().take(data.szTip.len() - 1).collect();
    data.szTip = [0; 128];
    data.szTip[..w.len()].copy_from_slice(&w);
}

fn add_tray(hwnd: HWND) {
    let data = tray_data(hwnd);
    let ok = unsafe { Shell_NotifyIconW(NIM_ADD, &data).as_bool() };
    STATE.with(|s| s.borrow_mut().tray_added = ok);
}

pub fn remove_tray() {
    let h = hwnd();
    if h.0.is_null() {
        return;
    }
    let data = tray_data(h);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

fn update_tray_tip(snap: &audio::Snapshot) {
    let text = if snap.has_device {
        format!(
            "{} — {}\n{}",
            util::APP_NAME,
            if snap.master.muted { tr("без звука", "muted").to_string() } else { format!("{}%", snap.master.volume as i32) },
            snap.device_name
        )
    } else {
        format!("{} — {}", util::APP_NAME, tr("нет устройства вывода", "no output device"))
    };
    let text: String = text.chars().take(127).collect();
    let changed = STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.tray_tip == text {
            false
        } else {
            s.tray_tip = text.clone();
            true
        }
    });
    if changed && STATE.with(|s| s.borrow().tray_added) {
        let mut data = tray_data(hwnd());
        data.uFlags = NIF_TIP;
        set_tip(&mut data, &text);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }
}

fn show_tray_menu(hwnd: HWND) {
    let s = settings::get();
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let add = |id: usize, text: &str, checked: bool| {
            let w = util::wide(text);
            let flags = if checked { MF_STRING | MF_CHECKED } else { MF_STRING };
            let _ = AppendMenuW(menu, flags, id, PCWSTR(w.as_ptr()));
        };
        add(1, tr("Микшер громкости", "Volume mixer"), false);
        add(2, tr("Остров вверху экрана", "Island at the top"), s.island.enabled);
        add(3, tr("Текст песни на рабочем столе", "Lyrics on the desktop"), s.desktop_lyrics);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        add(7, tr("Скриншот области", "Region screenshot"), false);
        add(8, if crate::tools::record::is_recording() { tr("Остановить запись", "Stop recording") } else { tr("Запись экрана", "Record screen") }, false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        add(4, tr("Горячие клавиши", "Hotkeys"), false);
        add(5, tr("Настройки", "Settings"), false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        add(6, tr("Выйти из OmniFlex", "Quit OmniFlex"), false);

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, None);
        let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match cmd.0 {
            1 => crate::ui::show_main(crate::ui::Page::Mixer),
            2 => settings::update(|s| s.island.enabled = !s.island.enabled),
            3 => settings::update(|s| s.desktop_lyrics = !s.desktop_lyrics),
            4 => crate::ui::show_main(crate::ui::Page::Hotkeys),
            5 => crate::ui::show_main(crate::ui::Page::Settings),
            6 => crate::exit_app(),
            7 => crate::tools::screenshot_region(),
            8 => crate::tools::toggle_record(),
            _ => {}
        }
    }
}
