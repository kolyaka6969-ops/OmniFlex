//! Играет ли пользователь прямо сейчас: на переднем плане игра (в том числе в окне, как Minecraft) или любое
//! полноэкранное окно — фильм, презентация. Остров в это время не показывается.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HMODULE, HWND, RECT};
use windows::Win32::System::ProcessStatus::{EnumProcessModulesEx, GetModuleBaseNameW, LIST_MODULES_ALL};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};
use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GWL_STYLE};
use windows::Win32::System::Registry::HKEY_CURRENT_USER;

use crate::win;

/// Не игру перепроверяем: игровой движок мог загрузиться позже окна (лаунчер Minecraft, заставка).
const RECHECK_DELAY: Duration = Duration::from_secs(15);
/// Список игр Xbox Game Bar перечитываем не чаще раза в минуту.
const GAME_BAR_REFRESH: Duration = Duration::from_secs(60);

/// Папки магазинов игр. Лаунчеры (Steam, Epic Games Launcher) лежат вне их и играми не считаются.
const GAME_FOLDERS: &[&str] = &[
    r"\steamapps\common\",
    r"\epic games\",
    r"\gog galaxy\games\",
    r"\gog games\",
    r"\riot games\",
    r"\xboxgames\",
    r"\ubisoft game launcher\games\",
    r"\ea games\",
    r"\origin games\",
    r"\games\",
];

const NOT_GAME_FOLDERS: &[&str] = &[r"\epic games\launcher\", r"\riot client\"];

/// Программы, которые никогда не считаются играми: Windows нередко заносит браузеры, Discord, плееры
/// или редакторы в список игр Xbox Game Bar.
const EXCLUDED_EXES: &[&str] = &[
    "chrome", "chromium", "msedge", "firefox", "opera", "brave", "vivaldi", "browser", "yandexbrowser", "thorium", "zen",
    "waterfox", "librewolf", "explorer", "devenv", "code", "spotify", "telegram", "discord", "steam",
    "epicgameslauncher", "antigravity", "taskmgr", "cmd", "powershell", "windowsterminal", "omniflex",
];

/// Библиотеки, которые грузят только игры: движки, игровые SDK магазинов, LWJGL у Minecraft.
const GAME_MODULES: &[&str] = &[
    "unityplayer.dll", "gameassembly.dll", "lwjgl.dll", "lwjgl64.dll", "lwjgl32.dll", "lwjgl_opengl.dll", "glfw.dll",
    "steam_api.dll", "steam_api64.dll", "eossdk-win64-shipping.dll", "eossdk-win32-shipping.dll", "galaxy.dll",
    "galaxy64.dll", "fmod.dll", "fmod64.dll", "fmodstudio.dll", "fmodstudio64.dll", "bink2w64.dll", "binkw32.dll",
    "binkw64.dll", "physx3_x64.dll", "physx_64.dll", "gameoverlayrenderer64.dll",
];

/// Окна рабочего стола и панели задач: на них остров нужен.
const SHELL_CLASSES: &[&str] = &[
    "Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd", "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow", "TopLevelWindowForOverflowXamlIsland",
];

pub struct GameDetector {
    processes: HashMap<u32, (String, bool, Instant)>,
    game_bar: HashSet<String>,
    game_bar_read: Option<Instant>,
    last_hwnd: isize,
    last_result: bool,
    last_time: Option<Instant>,
}

impl GameDetector {
    pub fn new() -> Self {
        Self {
            processes: HashMap::new(),
            game_bar: HashSet::new(),
            game_bar_read: None,
            last_hwnd: 0,
            last_result: false,
            last_time: None,
        }
    }

    /// Сейчас на переднем плане игра или полноэкранное окно.
    pub fn is_game_in_front(&mut self) -> bool {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.0 as isize == self.last_hwnd
            && self.last_time.map(|t| t.elapsed() < Duration::from_millis(1500)).unwrap_or(false)
        {
            return self.last_result;
        }

        let mut game = false;
        let state = unsafe { SHQueryUserNotificationState() };
        if let Ok(state) = state {
            if state == QUNS_RUNNING_D3D_FULL_SCREEN || state == QUNS_PRESENTATION_MODE {
                game = true;
            }
        }
        if !game && !hwnd.0.is_null() && !SHELL_CLASSES.contains(&win::window_class(hwnd).as_str()) {
            let pid = win::window_pid(hwnd);
            if pid != 0 && pid != win::own_pid() {
                game = is_fullscreen(hwnd) || self.is_game_process(pid);
            }
        }
        self.last_hwnd = hwnd.0 as isize;
        self.last_result = game;
        self.last_time = Some(Instant::now());
        game
    }

    fn is_game_process(&mut self, pid: u32) -> bool {
        let Some(path) = win::process_path(pid) else { return false };
        let exe = win::file_stem(&path).to_lowercase();
        if EXCLUDED_EXES.contains(&exe.as_str()) {
            return false;
        }
        // Номер процесса мог достаться другой программе — тогда путь другой
        if let Some((cached_path, game, checked)) = self.processes.get(&pid) {
            if *cached_path == path && (*game || checked.elapsed() < RECHECK_DELAY) {
                return *game;
            }
        }
        let game = is_game_path(&path) || self.is_game_bar_game(&path) || has_game_modules(pid);
        if self.processes.len() > 256 {
            self.processes.clear();
        }
        self.processes.insert(pid, (path, game, Instant::now()));
        game
    }

    /// Xbox Game Bar сам ведёт список программ, которые он счёл играми.
    fn is_game_bar_game(&mut self, path: &str) -> bool {
        if self.game_bar_read.map(|t| t.elapsed() > GAME_BAR_REFRESH).unwrap_or(true) {
            self.game_bar_read = Some(Instant::now());
            let mut set = HashSet::new();
            let base = r"System\GameConfigStore\Children";
            for child in registry_subkeys(base) {
                if let Some(exe) = win::reg_read_string(HKEY_CURRENT_USER, &format!(r"{base}\{child}"), "MatchedExeFullPath") {
                    if !exe.is_empty() {
                        set.insert(exe.to_lowercase());
                    }
                }
            }
            self.game_bar = set;
        }
        self.game_bar.contains(&path.to_lowercase())
    }
}

fn registry_subkeys(path: &str) -> Vec<String> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::System::Registry::{RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, HKEY, KEY_READ};
    let mut names = Vec::new();
    let path_w = crate::util::wide(path);
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path_w.as_ptr()), 0, KEY_READ, &mut key).is_err() {
            return names;
        }
        let mut i = 0;
        loop {
            let mut buf = vec![0u16; 256];
            let mut len = buf.len() as u32;
            if RegEnumKeyExW(key, i, PWSTR(buf.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None).is_err() {
                break;
            }
            names.push(String::from_utf16_lossy(&buf[..len as usize]));
            i += 1;
        }
        let _ = RegCloseKey(key);
    }
    names
}

/// Окно без заголовка на весь монитор: игра «в окне без рамки», видео на весь экран. Окно с заголовком
/// не считается — развёрнутый браузер при скрытой панели задач тоже закрывает весь монитор.
fn is_fullscreen(hwnd: HWND) -> bool {
    const WS_CAPTION: isize = 0x00C0_0000;
    unsafe {
        if GetWindowLongPtrW(hwnd, GWL_STYLE) & WS_CAPTION == WS_CAPTION {
            return false;
        }
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect).is_err() {
            return false;
        }
        let m = win::monitor_of_window(hwnd).bounds;
        rect.left <= m.left && rect.top <= m.top && rect.right >= m.right && rect.bottom >= m.bottom
    }
}

/// Игра по пути к exe: из библиотеки Steam, Epic, GOG и других магазинов или собранная на Unreal.
pub fn is_game_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    let name = win::file_stem(&lower);
    if EXCLUDED_EXES.contains(&name.as_str()) {
        return false;
    }
    if NOT_GAME_FOLDERS.iter().any(|f| lower.contains(f)) {
        return false;
    }
    // Unreal Engine называет exe игры «Имя-Win64-Shipping.exe»
    name.ends_with("-win64-shipping") || name.ends_with("-wingdk-shipping") || GAME_FOLDERS.iter().any(|f| lower.contains(f))
}

fn has_game_modules(pid: u32) -> bool {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid) else { return false };
        let mut modules = vec![HMODULE::default(); 1024];
        let mut needed = 0u32;
        let ok = EnumProcessModulesEx(
            handle,
            modules.as_mut_ptr(),
            (modules.len() * std::mem::size_of::<HMODULE>()) as u32,
            &mut needed,
            LIST_MODULES_ALL,
        )
        .is_ok();
        let mut found = false;
        if ok {
            let count = (needed as usize / std::mem::size_of::<HMODULE>()).min(modules.len());
            let mut name = [0u16; 260];
            for m in &modules[..count] {
                let len = GetModuleBaseNameW(handle, *m, &mut name);
                if len > 0 {
                    let s = String::from_utf16_lossy(&name[..len as usize]).to_lowercase();
                    if GAME_MODULES.contains(&s.as_str()) {
                        found = true;
                        break;
                    }
                }
            }
        }
        let _ = CloseHandle(handle);
        found
    }
}
