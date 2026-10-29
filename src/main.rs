//! OmniFlex — остров вверху экрана и микшер громкости для Windows 10/11.
//! Потоки: главный — окно настроек (egui); поток окон — трей, горячие клавиши, микшер, остров, индикатор;
//! поток ядра — медиа-сеансы и текст песни на рабочем столе; поток обоев — окна в слое рабочего стола.

#![windows_subsystem = "windows"]

mod agents;
mod ai;
mod audio;
mod core;
mod desktop;
mod eq;
mod flyout;
mod game;
mod gfx;
mod hotkeys;
mod i18n;
mod island;
mod launcher;
mod media;
mod notify;
mod osd;
mod process_info;
mod prompt;
mod reminders;
mod settings;
mod shell;
mod sysinfo;
mod theme;
mod tools;
mod ui;
mod util;
mod voice;
mod win;
mod wnd;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::System::Threading::{CreateEventW, CreateMutexW, OpenEventW, SetEvent, WaitForSingleObject, EVENT_MODIFY_STATE, INFINITE};
use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;

const MUTEX_NAME: &str = "OmniFlex_SingleInstance";
const SHOW_EVENT_NAME: &str = "OmniFlex_ShowWindow";

fn main() {
    std::panic::set_hook(Box::new(|info| util::log_error(format!("panic: {info}"))));
    // Хуки агентов и команды из командной строки: передать запущенной копии и выйти
    let args: Vec<String> = std::env::args().collect();
    if agents::run_cli(&args) {
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--render-test") {
        settings::load();
        let dir = std::path::PathBuf::from(args.get(i + 1).cloned().unwrap_or_else(|| ".".into()));
        island::render_test(&dir, args.get(i + 2).map(std::path::PathBuf::from));
        return;
    }
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }

    // Уже запущено — просим первую копию показать окно
    let mutex_name = util::wide(MUTEX_NAME);
    let event_name = util::wide(SHOW_EVENT_NAME);
    unsafe {
        let _mutex = CreateMutexW(None, true, PCWSTR(mutex_name.as_ptr()));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = AllowSetForegroundWindow(u32::MAX);
            if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, PCWSTR(event_name.as_ptr())) {
                let _ = SetEvent(event);
            }
            return;
        }
        // Мьютекс держится до конца процесса
        std::mem::forget(_mutex);
    }

    settings::load();
    win::repair_autostart();
    settings::set_listener(|old, new| {
        shell::settings_changed(old.clone(), new.clone());
        core::post(core::Event::Settings(new.clone()));
        ui::request_repaint();
    });

    shell::start();
    core::start();
    agents::start_server();
    listen_show_event(event_name);

    let tray = std::env::args().any(|a| a.eq_ignore_ascii_case(win::TRAY_ARGUMENT));
    ui::run(tray);
    exit_app();
}

/// Второй запуск программы ставит событие — показываем окно.
fn listen_show_event(name: Vec<u16>) {
    let event: Option<HANDLE> = unsafe { CreateEventW(None, false, false, PCWSTR(name.as_ptr())).ok() };
    let Some(event) = event else { return };
    let raw = event.0 as isize;
    std::thread::Builder::new()
        .name("OmniFlex show listener".into())
        .spawn(move || loop {
            unsafe {
                WaitForSingleObject(HANDLE(raw as _), INFINITE);
            }
            ui::show_main(ui::Page::Mixer);
        })
        .ok();
}

/// Выход: убрать значок из трея и обои с песней (Explorer вернёт обычные), затем завершить процесс.
pub fn exit_app() {
    static EXITING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if EXITING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        core::shutdown_blocking();
        let (tx, rx) = std::sync::mpsc::channel();
        shell::post(move || {
            shell::remove_tray();
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(std::time::Duration::from_secs(2));
        std::process::exit(0);
    });
}
