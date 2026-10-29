//! Глобальные горячие клавиши через RegisterHotKey на скрытом окне потока окон.

use std::collections::HashMap;
use std::sync::Mutex;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
};

use crate::settings::{HotkeyBinding, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN};

/// Клавиши, которые можно назначить без модификаторов: F1–F24, мультимедийные и т. п.
pub fn is_standalone_key(vk: u32) -> bool {
    matches!(vk,
        0x70..=0x87          // F1–F24
        | 0x13 | 0x91 | 0x2D // Pause, Scroll Lock, Insert
        | 0xAD..=0xAF        // без звука, тише, громче
        | 0xB0..=0xB3        // следующий, предыдущий, стоп, играть/пауза
        | 0xB6 | 0xB7)       // запуск приложений
}

pub fn is_modifier(vk: u32) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

pub fn key_name(vk: u32) -> String {
    match vk {
        0x21 => "PageUp".into(),
        0x22 => "PageDown".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        0x0D => "Enter".into(),
        0x14 => "CapsLock".into(),
        0x1B => "Esc".into(),
        0x20 => "Пробел".into(),
        0x08 => "Backspace".into(),
        0x09 => "Tab".into(),
        0x13 => "Pause".into(),
        0x91 => "ScrollLock".into(),
        0x2C => "PrintScreen".into(),
        0x26 => "↑".into(),
        0x28 => "↓".into(),
        0x25 => "←".into(),
        0x27 => "→".into(),
        0xBB => "=".into(),
        0xBD => "-".into(),
        0xBC => ",".into(),
        0xBE => ".".into(),
        0xBF => "/".into(),
        0xBA => ";".into(),
        0xDE => "'".into(),
        0xDB => "[".into(),
        0xDD => "]".into(),
        0xDC => "\\".into(),
        0xC0 => "`".into(),
        0x6B => "Num +".into(),
        0x6D => "Num -".into(),
        0x6A => "Num *".into(),
        0x6F => "Num /".into(),
        0x6E => "Num .".into(),
        0xAD => "Без звука".into(),
        0xAE => "Громкость −".into(),
        0xAF => "Громкость +".into(),
        0xB0 => "Следующий трек".into(),
        0xB1 => "Предыдущий трек".into(),
        0xB2 => "Стоп".into(),
        0xB3 => "Играть/пауза".into(),
        0xB6 => "Приложение 1".into(),
        0xB7 => "Приложение 2".into(),
        0x30..=0x39 => ((b'0' + (vk - 0x30) as u8) as char).to_string(),
        0x41..=0x5A => ((b'A' + (vk - 0x41) as u8) as char).to_string(),
        0x60..=0x69 => format!("Num {}", vk - 0x60),
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        _ => format!("Клавиша {vk:#04X}"),
    }
}

pub fn format(modifiers: u32, vk: u32) -> String {
    if vk == 0 {
        return String::new();
    }
    let mut parts: Vec<String> = Vec::new();
    if modifiers & MOD_WIN != 0 {
        parts.push("Win".into());
    }
    if modifiers & MOD_CONTROL != 0 {
        parts.push("Ctrl".into());
    }
    if modifiers & MOD_ALT != 0 {
        parts.push("Alt".into());
    }
    if modifiers & MOD_SHIFT != 0 {
        parts.push("Shift".into());
    }
    parts.push(key_name(vk));
    parts.join(" + ")
}

fn down(vk: i32) -> bool {
    unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
}

/// Нажатые сейчас модификаторы — для захвата сочетания в настройках.
pub fn current_modifiers() -> u32 {
    let mut m = 0;
    if down(0x11) {
        m |= MOD_CONTROL;
    }
    if down(0x12) {
        m |= MOD_ALT;
    }
    if down(0x10) {
        m |= MOD_SHIFT;
    }
    if down(0x5B) || down(0x5C) {
        m |= MOD_WIN;
    }
    m
}

/// Первая нажатая сейчас клавиша, кроме модификаторов и кнопок мыши.
pub fn pressed_key() -> Option<u32> {
    (0x08u32..=0xFE).find(|&vk| !is_modifier(vk) && !matches!(vk, 0x01..=0x06) && down(vk as i32))
}

/// Привязки, которые не удалось зарегистрировать (сочетание занято другой программой).
pub static FAILED: Mutex<Vec<HotkeyBinding>> = Mutex::new(Vec::new());

/// Зарегистрированные сочетания: номер → привязка.
pub struct Registry {
    hwnd: HWND,
    registered: HashMap<i32, HotkeyBinding>,
    bindings: Vec<HotkeyBinding>,
    suspended: bool,
}

impl Registry {
    pub fn new(hwnd: HWND) -> Self {
        Self { hwnd, registered: HashMap::new(), bindings: Vec::new(), suspended: false }
    }

    pub fn apply(&mut self, bindings: &[HotkeyBinding]) {
        self.bindings = bindings.to_vec();
        if !self.suspended {
            self.register_all();
        }
    }

    /// Снимает хоткеи на время захвата нового сочетания в настройках.
    pub fn suspend(&mut self, suspend: bool) {
        if self.suspended == suspend {
            return;
        }
        self.suspended = suspend;
        if suspend {
            self.unregister_all();
        } else {
            self.register_all();
        }
    }

    pub fn get(&self, id: i32) -> Option<&HotkeyBinding> {
        self.registered.get(&id)
    }

    fn register_all(&mut self) {
        self.unregister_all();
        let mut failed = Vec::new();
        let mut id = 1;
        for b in &self.bindings {
            // Одиночная буква сломала бы набор текста во всех программах
            if b.vk == 0 || (b.modifiers == 0 && !is_standalone_key(b.vk)) {
                continue;
            }
            // Без MOD_NOREPEAT: зажатая «громче» повторяется, как и должна
            let mods = HOT_KEY_MODIFIERS(b.modifiers);
            let ok = unsafe { RegisterHotKey(self.hwnd, id, mods, b.vk).is_ok() };
            if ok {
                self.registered.insert(id, b.clone());
                id += 1;
            } else {
                failed.push(b.clone());
            }
        }
        if let Ok(mut f) = FAILED.lock() {
            *f = failed;
        }
    }

    fn unregister_all(&mut self) {
        for id in self.registered.keys() {
            unsafe {
                let _ = UnregisterHotKey(self.hwnd, *id);
            }
        }
        self.registered.clear();
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        self.unregister_all();
    }
}
