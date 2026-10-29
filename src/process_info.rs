//! Сведения о программе по номеру процесса: имя exe (по нему работают горячие клавиши), название из
//! свойств файла и значок.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use windows::core::PCWSTR;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, DIB_RGB_COLORS, HGDIOBJ,
};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW, FILE_FLAGS_AND_ATTRIBUTES,
};
use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows::Win32::Foundation::HWND;

use crate::util::{from_wide, wide};
use crate::win;

/// Значок программы: RGBA без премультипликации.
#[derive(Debug)]
pub struct AppIcon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct ProcInfo {
    /// Имя exe без расширения.
    pub key: String,
    pub name: String,
    pub icon: Option<Arc<AppIcon>>,
}

fn icon_cache() -> &'static Mutex<HashMap<String, Option<Arc<AppIcon>>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<Arc<AppIcon>>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn get(pid: u32) -> Option<ProcInfo> {
    let path = win::process_path(pid)?;
    let key = win::file_stem(&path);
    let name = file_description(&path).filter(|d| !d.trim().is_empty()).map(|d| d.trim().to_string()).unwrap_or_else(|| key.clone());
    let icon = {
        let cached = icon_cache().lock().ok().and_then(|c| c.get(&path.to_lowercase()).cloned());
        match cached {
            Some(icon) => icon,
            None => {
                let icon = load_icon(&path).map(Arc::new);
                if let Ok(mut c) = icon_cache().lock() {
                    c.insert(path.to_lowercase(), icon.clone());
                }
                icon
            }
        }
    };
    Some(ProcInfo { key, name, icon })
}

/// Описание файла («Google Chrome», «Spotify») из его свойств.
pub fn file_description(path: &str) -> Option<String> {
    let path_w = wide(path);
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(path_w.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(path_w.as_ptr()), 0, size, buf.as_mut_ptr() as *mut _).ok()?;

        let mut ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut len = 0u32;
        let translation = wide("\\VarFileInfo\\Translation");
        let mut langs: Vec<(u16, u16)> = Vec::new();
        if VerQueryValueW(buf.as_ptr() as *const _, PCWSTR(translation.as_ptr()), &mut ptr, &mut len).as_bool()
            && !ptr.is_null()
        {
            let pairs = std::slice::from_raw_parts(ptr as *const u16, (len / 2) as usize);
            for chunk in pairs.chunks(2) {
                if chunk.len() == 2 {
                    langs.push((chunk[0], chunk[1]));
                }
            }
        }
        langs.push((0x0409, 0x04B0));
        langs.push((0x0409, 0x04E4));
        for (lang, cp) in langs {
            let query = wide(&format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription"));
            let mut p: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut l = 0u32;
            if VerQueryValueW(buf.as_ptr() as *const _, PCWSTR(query.as_ptr()), &mut p, &mut l).as_bool()
                && !p.is_null()
                && l > 0
            {
                let s = from_wide(std::slice::from_raw_parts(p as *const u16, l as usize));
                if !s.trim().is_empty() {
                    return Some(s);
                }
            }
        }
        None
    }
}

pub fn load_icon(path: &str) -> Option<AppIcon> {
    let path_w = wide(path);
    unsafe {
        let mut info = SHFILEINFOW::default();
        let ok = SHGetFileInfoW(
            PCWSTR(path_w.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if ok == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let icon = hicon_to_rgba(info.hIcon);
        let _ = DestroyIcon(info.hIcon);
        icon
    }
}

/// HICON в RGBA. У старых значков нет альфа-канала — тогда прозрачность берётся из маски.
pub fn hicon_to_rgba(icon: HICON) -> Option<AppIcon> {
    unsafe {
        let mut ii = ICONINFO::default();
        GetIconInfo(icon, &mut ii).ok()?;
        let color = ii.hbmColor;
        let mask = ii.hbmMask;
        let result = (|| {
            if color.is_invalid() {
                return None;
            }
            let mut bm = BITMAP::default();
            if GetObjectW(HGDIOBJ(color.0), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0
            {
                return None;
            }
            let (w, h) = (bm.bmWidth, bm.bmHeight);
            if w <= 0 || h <= 0 {
                return None;
            }
            let dc = GetDC(HWND::default());
            let mut info = crate::wnd::bitmap_info(w, h);
            let mut bgra = vec![0u8; (w * h * 4) as usize];
            let lines = GetDIBits(dc, color, 0, h as u32, Some(bgra.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
            let mut has_alpha = bgra.chunks(4).any(|p| p[3] != 0);
            if !has_alpha && !mask.is_invalid() {
                let mut mask_bits = vec![0u8; (w * h * 4) as usize];
                let mut minfo = crate::wnd::bitmap_info(w, h);
                if GetDIBits(dc, mask, 0, h as u32, Some(mask_bits.as_mut_ptr() as *mut _), &mut minfo, DIB_RGB_COLORS) > 0 {
                    for (p, m) in bgra.chunks_mut(4).zip(mask_bits.chunks(4)) {
                        p[3] = if m[0] == 0 { 255 } else { 0 };
                    }
                    has_alpha = true;
                }
            }
            ReleaseDC(HWND::default(), dc);
            if lines == 0 || !has_alpha {
                return None;
            }
            let mut rgba = bgra;
            for p in rgba.chunks_mut(4) {
                p.swap(0, 2);
            }
            Some(AppIcon { width: w as u32, height: h as u32, rgba })
        })();
        if !color.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(color.0));
        }
        if !mask.is_invalid() {
            let _ = DeleteObject(HGDIOBJ(mask.0));
        }
        result
    }
}
