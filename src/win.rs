//! Обёртки над WinAPI, нужные в разных местах: процессы, окна, мониторы, реестр.

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, HDC, HMONITOR, MONITORINFO,
    MONITORINFOEXW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetWindowTextW, GetWindowThreadProcessId, SystemParametersInfoW, SPI_GETDESKWALLPAPER,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

use crate::util::{from_wide, wide};

/// Полный путь к exe процесса; None — процесс завершён или недоступен.
pub fn process_path(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(handle);
        if ok {
            Some(String::from_utf16_lossy(&buf[..len as usize]))
        } else {
            None
        }
    }
}

/// Имя файла без расширения.
pub fn file_stem(path: &str) -> String {
    std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid
}

pub fn window_class(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    if len <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

pub fn window_title(hwnd: HWND) -> String {
    let mut buf = vec![0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if len <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..len as usize])
    }
}

pub fn own_pid() -> u32 {
    std::process::id()
}

/// Монитор: имя устройства (\\.\DISPLAY2), границы, рабочая область, основной ли.
#[derive(Clone, Debug, PartialEq)]
pub struct Monitor {
    pub handle: isize,
    pub device: String,
    pub bounds: RECT,
    pub work: RECT,
    pub primary: bool,
}

pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn callback(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = &mut *(data.0 as *mut Vec<Monitor>);
        if let Some(m) = monitor_info(monitor) {
            list.push(m);
        }
        true.into()
    }
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(HDC::default(), None, Some(callback), LPARAM(&mut list as *mut _ as isize));
    }
    list
}

pub fn monitor_info(monitor: HMONITOR) -> Option<Monitor> {
    unsafe {
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(monitor, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
            return None;
        }
        Some(Monitor {
            handle: monitor.0 as isize,
            device: from_wide(&info.szDevice),
            bounds: info.monitorInfo.rcMonitor,
            work: info.monitorInfo.rcWork,
            primary: info.monitorInfo.dwFlags & 1 != 0,
        })
    }
}

pub fn primary_monitor() -> Monitor {
    unsafe {
        monitor_info(MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY)).unwrap_or(Monitor {
            handle: 0,
            device: String::new(),
            bounds: RECT { left: 0, top: 0, right: 1920, bottom: 1080 },
            work: RECT { left: 0, top: 0, right: 1920, bottom: 1040 },
            primary: true,
        })
    }
}

/// Монитор по имени устройства; если его отключили (или имя пустое) — основной.
pub fn monitor_by_device(device: &str) -> Monitor {
    if !device.is_empty() {
        if let Some(m) = monitors().into_iter().find(|m| m.device == device) {
            return m;
        }
    }
    primary_monitor()
}

pub fn monitor_at(pt: POINT) -> Monitor {
    unsafe { monitor_info(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST)).unwrap_or_else(primary_monitor) }
}

pub fn monitor_of_window(hwnd: HWND) -> Monitor {
    unsafe { monitor_info(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)).unwrap_or_else(primary_monitor) }
}

/// Масштаб монитора (1.0 = 96 dpi).
pub fn monitor_scale(monitor: &Monitor) -> f32 {
    let mut x = 96u32;
    let mut y = 96u32;
    unsafe {
        if GetDpiForMonitor(HMONITOR(monitor.handle as _), MDT_EFFECTIVE_DPI, &mut x, &mut y).is_err() {
            return 1.0;
        }
    }
    x as f32 / 96.0
}

pub fn wallpaper_path() -> Option<String> {
    let mut buf = vec![0u16; 1024];
    unsafe {
        SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            buf.len() as u32,
            Some(buf.as_mut_ptr() as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
        .ok()?;
    }
    let s = from_wide(&buf);
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

// ---------- Реестр ----------

pub fn reg_read_string(key: HKEY, subkey: &str, name: &str) -> Option<String> {
    let subkey = wide(subkey);
    let name = wide(name);
    let mut size = 0u32;
    unsafe {
        RegGetValueW(key, PCWSTR(subkey.as_ptr()), PCWSTR(name.as_ptr()), RRF_RT_REG_SZ, None, None, Some(&mut size))
            .ok()
            .ok()?;
        let mut buf = vec![0u16; (size as usize / 2) + 1];
        let mut size2 = (buf.len() * 2) as u32;
        RegGetValueW(
            key,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size2),
        )
        .ok()
        .ok()?;
        Some(from_wide(&buf))
    }
}

pub fn reg_read_dword(key: HKEY, subkey: &str, name: &str) -> Option<u32> {
    let subkey = wide(subkey);
    let name = wide(name);
    let mut value = 0u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(
            key,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
        .ok()
        .ok()?;
    }
    Some(value)
}

pub fn reg_write_string(key: HKEY, subkey: &str, name: &str, value: &str) -> bool {
    let subkey = wide(subkey);
    let name = wide(name);
    let data = wide(value);
    unsafe {
        let mut hkey = HKEY::default();
        if RegCreateKeyExW(
            key,
            PCWSTR(subkey.as_ptr()),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        )
        .is_err()
        {
            return false;
        }
        let bytes = std::slice::from_raw_parts(data.as_ptr() as *const u8, data.len() * 2);
        let ok = RegSetValueExW(hkey, PCWSTR(name.as_ptr()), 0, REG_SZ, Some(bytes)).is_ok();
        let _ = RegCloseKey(hkey);
        ok
    }
}

pub fn reg_delete_value(key: HKEY, subkey: &str, name: &str) {
    let subkey = wide(subkey);
    let name = wide(name);
    unsafe {
        let mut hkey = HKEY::default();
        if RegCreateKeyExW(
            key,
            PCWSTR(subkey.as_ptr()),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        )
        .is_ok()
        {
            let _ = RegDeleteValueW(hkey, PCWSTR(name.as_ptr()));
            let _ = RegCloseKey(hkey);
        }
    }
}

/// Светлая тема приложений в Windows.
pub fn system_light_theme() -> bool {
    reg_read_dword(
        HKEY_CURRENT_USER,
        r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize",
        "AppsUseLightTheme",
    ) == Some(1)
}

/// Цвет акцента Windows (RGB), если удалось прочитать.
pub fn accent_color() -> Option<[u8; 3]> {
    let v = reg_read_dword(HKEY_CURRENT_USER, r"Software\Microsoft\Windows\DWM", "AccentColor")?;
    // ABGR
    Some([(v & 0xFF) as u8, ((v >> 8) & 0xFF) as u8, ((v >> 16) & 0xFF) as u8])
}

// ---------- Автозапуск через HKCU\...\Run (тот же ключ использует установщик) ----------

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const TRAY_ARGUMENT: &str = "--tray";

fn autostart_command() -> String {
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    format!("\"{exe}\" {TRAY_ARGUMENT}")
}

pub fn autostart_enabled() -> bool {
    reg_read_string(HKEY_CURRENT_USER, RUN_KEY, crate::util::APP_NAME).is_some()
}

pub fn set_autostart(enabled: bool) {
    if enabled {
        reg_write_string(HKEY_CURRENT_USER, RUN_KEY, crate::util::APP_NAME, &autostart_command());
    } else {
        reg_delete_value(HKEY_CURRENT_USER, RUN_KEY, crate::util::APP_NAME);
    }
}

/// Если exe переехал (например, после переустановки в другую папку) — обновляем путь.
pub fn repair_autostart() {
    if let Some(value) = reg_read_string(HKEY_CURRENT_USER, RUN_KEY, crate::util::APP_NAME) {
        let command = autostart_command();
        if value != command {
            reg_write_string(HKEY_CURRENT_USER, RUN_KEY, crate::util::APP_NAME, &command);
        }
    }
}

/// Программа обоев (Wallpaper Engine, Lively): над её окнами кнопки на обоях тоже работают.
pub fn is_wallpaper_process(pid: u32) -> bool {
    if pid == 0 || pid == own_pid() {
        return false;
    }
    match process_path(pid) {
        Some(path) => {
            let exe = file_stem(&path).to_lowercase();
            exe.contains("wallpaper") || exe.contains("lively")
        }
        None => false,
    }
}
