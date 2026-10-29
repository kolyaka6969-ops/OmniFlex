//! Что показывает остров: заряд батареи, раскладка клавиатуры, Caps Lock, кто сейчас слушает микрофон
//! и смотрит в камеру.

use windows::Win32::Foundation::HWND;
use windows::Win32::Globalization::{GetLocaleInfoW, LOCALE_SISO639LANGNAME};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, GetKeyboardLayout};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
use windows::core::{PCWSTR, PWSTR};

use crate::util::{from_wide, wide};
use crate::win;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
    pub plugged: bool,
}

/// None — батареи нет (настольный компьютер).
pub fn battery() -> Option<Battery> {
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe {
        GetSystemPowerStatus(&mut status).ok()?;
    }
    if status.BatteryFlag & 128 != 0 || status.BatteryFlag == 255 || status.BatteryLifePercent > 100 {
        return None;
    }
    Some(Battery {
        percent: status.BatteryLifePercent,
        charging: status.BatteryFlag & 8 != 0,
        plugged: status.ACLineStatus == 1,
    })
}

/// Раскладка активного окна: «RU», «EN» и т. п.
pub fn keyboard_layout() -> String {
    unsafe {
        let hwnd = GetForegroundWindow();
        let thread = GetWindowThreadProcessId(hwnd, None);
        let hkl = GetKeyboardLayout(thread);
        let lang = (hkl.0 as usize & 0xFFFF) as u32;
        let mut buf = [0u16; 16];
        let len = GetLocaleInfoW(lang, LOCALE_SISO639LANGNAME, Some(&mut buf));
        if len > 0 {
            from_wide(&buf).to_uppercase()
        } else {
            format!("{lang:04X}")
        }
    }
}

/// Caps Lock включён. Состояние переключателя общее, но поток видит его свежим только при вводе, поэтому
/// окна острова получают сырой ввод клавиатуры (RIDEV_INPUTSINK) и спрашивают снова на каждое нажатие.
pub fn caps_lock() -> bool {
    unsafe { GetKeyState(0x14) & 1 != 0 }
}

/// Программа, которая сейчас пользуется микрофоном или камерой: имя из ключа реестра CapabilityAccessManager.
pub fn devices_in_use(kind: &str) -> Vec<String> {
    let base = format!(r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\{kind}");
    let mut users = Vec::new();
    collect_in_use(&base, false, &mut users);
    collect_in_use(&format!(r"{base}\NonPackaged"), true, &mut users);
    users.sort();
    users.dedup();
    users
}

fn collect_in_use(path: &str, non_packaged: bool, out: &mut Vec<String>) {
    for name in subkeys(HKEY_CURRENT_USER, path) {
        if name == "NonPackaged" {
            continue;
        }
        let key = format!(r"{path}\{name}");
        let start = read_qword(&key, "LastUsedTimeStart").unwrap_or(0);
        let stop = read_qword(&key, "LastUsedTimeStop").unwrap_or(1);
        if start > 0 && stop == 0 {
            out.push(friendly(&name, non_packaged));
        }
    }
}

/// «C:#Program Files#Discord#Discord.exe» → «Discord»; «Microsoft.WindowsCamera_8wekyb3d8bbwe» → «WindowsCamera».
fn friendly(name: &str, non_packaged: bool) -> String {
    if non_packaged {
        let path = name.replace('#', "\\");
        let stem = win::file_stem(&path);
        if let Some(desc) = crate::process_info::file_description(&path) {
            if !desc.trim().is_empty() {
                return desc.trim().to_string();
            }
        }
        return stem;
    }
    let base = name.split('_').next().unwrap_or(name);
    base.rsplit('.').next().unwrap_or(base).to_string()
}

fn subkeys(root: HKEY, path: &str) -> Vec<String> {
    let mut names = Vec::new();
    let path_w = wide(path);
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(root, PCWSTR(path_w.as_ptr()), 0, KEY_READ, &mut key).is_err() {
            return names;
        }
        let mut i = 0u32;
        loop {
            let mut buf = vec![0u16; 512];
            let mut len = buf.len() as u32;
            let r = RegEnumKeyExW(key, i, PWSTR(buf.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None);
            if r.is_err() {
                break;
            }
            names.push(String::from_utf16_lossy(&buf[..len as usize]));
            i += 1;
        }
        let _ = RegCloseKey(key);
    }
    names
}

fn read_qword(path: &str, name: &str) -> Option<u64> {
    use windows::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_QWORD};
    let path_w = wide(path);
    let name_w = wide(name);
    let mut value = 0u64;
    let mut size = 8u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(path_w.as_ptr()),
            PCWSTR(name_w.as_ptr()),
            RRF_RT_REG_QWORD,
            None,
            Some(&mut value as *mut u64 as *mut _),
            Some(&mut size),
        )
        .ok()
        .ok()?;
    }
    Some(value)
}

/// Окно на переднем плане (для «активного окна» в горячих клавишах).
pub fn foreground() -> HWND {
    unsafe { GetForegroundWindow() }
}
