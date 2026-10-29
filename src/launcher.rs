//! Лаунчер острова: ярлыки программ, папок и файлов, ссылки на сайты и команды других программ
//! (steam://run/…), группы. Значки подтягиваются сами: у файла — его значок, у сайта — фавикон,
//! у команды — значок программы, которая её открывает; или берутся из выбранного файла.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use windows::core::PCWSTR;
use windows::Win32::System::Registry::HKEY_CLASSES_ROOT;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::settings::{self, LauncherItem};
use crate::util::wide;

thread_local! {
    static ICONS: RefCell<HashMap<String, Option<Rc<tiny_skia::Pixmap>>>> = RefCell::new(HashMap::new());
    static LOADING: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

fn icons_dir() -> PathBuf {
    crate::util::data_dir().join("icons")
}

fn cache_key(item: &LauncherItem) -> String {
    format!("{}|{}", item.target, item.icon)
}

/// Значок ярлыка (поток окон). None — значка нет или он ещё загружается.
pub fn icon(item: &LauncherItem) -> Option<Rc<tiny_skia::Pixmap>> {
    let key = cache_key(item);
    if let Some(found) = ICONS.with(|c| c.borrow().get(&key).cloned()) {
        return found;
    }
    let loaded = load(item);
    if loaded.is_none() && item.is_link() && item.icon.is_empty() && is_web(&item.target) {
        // Фавикон ещё не скачан: качаем в фоне и не запоминаем «нет значка»
        fetch_favicon(item.target.clone());
        return None;
    }
    let loaded = loaded.map(Rc::new);
    ICONS.with(|c| c.borrow_mut().insert(key, loaded.clone()));
    loaded
}

/// Значки устарели (поменяли ярлык).
pub fn forget(item: &LauncherItem) {
    ICONS.with(|c| c.borrow_mut().remove(&cache_key(item)));
}

fn is_web(target: &str) -> bool {
    let l = target.to_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

fn host(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    rest.split(['/', '?', '#']).next().unwrap_or(rest).to_lowercase()
}

fn favicon_path(url: &str) -> PathBuf {
    icons_dir().join(format!("{}.png", crate::media::lyrics::safe_file_name(&host(url))))
}

fn decode_file(path: &Path) -> Option<tiny_skia::Pixmap> {
    let bytes = std::fs::read(path).ok()?;
    crate::gfx::decode_image(&bytes, 96)
}

fn exe_icon(path: &str) -> Option<tiny_skia::Pixmap> {
    let icon = crate::process_info::load_icon(path)?;
    crate::gfx::icon_to_pixmap(icon.width, icon.height, &icon.rgba)
}

fn load(item: &LauncherItem) -> Option<tiny_skia::Pixmap> {
    if !item.icon.trim().is_empty() {
        let p = item.icon.trim();
        let lower = p.to_lowercase();
        if lower.ends_with(".exe") || lower.ends_with(".dll") || lower.ends_with(".lnk") {
            return exe_icon(p);
        }
        return decode_file(Path::new(p)).or_else(|| exe_icon(p));
    }
    if item.is_group() {
        return None;
    }
    if is_web(&item.target) {
        return decode_file(&favicon_path(&item.target));
    }
    if item.is_link() {
        // Команда другой программы: значок того, что открывает её схему
        let scheme = item.target.split("://").next().unwrap_or("");
        return handler_exe(scheme).and_then(|exe| exe_icon(&exe));
    }
    exe_icon(&item.target)
}

/// Программа, которая открывает ссылки схемы (steam, discord, …).
fn handler_exe(scheme: &str) -> Option<String> {
    let from = |sub: &str| crate::win::reg_read_string(HKEY_CLASSES_ROOT, sub, "");
    let raw = from(&format!(r"{scheme}\DefaultIcon")).or_else(|| from(&format!(r"{scheme}\shell\open\command")))?;
    let raw = raw.trim();
    let path = if let Some(rest) = raw.strip_prefix('"') {
        rest.split('"').next().unwrap_or("").to_string()
    } else {
        raw.split(',').next().unwrap_or("").split(" -").next().unwrap_or("").trim().to_string()
    };
    if path.is_empty() {
        None
    } else {
        Some(path)
    }
}

fn fetch_favicon(url: String) {
    let h = host(&url);
    let fresh = LOADING.with(|l| l.borrow_mut().insert(h.clone()));
    if !fresh {
        return;
    }
    crate::media::runtime().spawn(async move {
        let api = format!("https://www.google.com/s2/favicons?domain={}&sz=64", crate::media::escape(&h));
        let bytes = match crate::media::fallback().get(api).send().await {
            Ok(r) if r.status().is_success() => r.bytes().await.ok(),
            _ => None,
        };
        if let Some(b) = bytes {
            let _ = std::fs::create_dir_all(icons_dir());
            let _ = std::fs::write(favicon_path(&url), &b);
        }
        crate::shell::post(move || {
            LOADING.with(|l| l.borrow_mut().remove(&h));
            ICONS.with(|c| c.borrow_mut().retain(|k, _| !k.contains(&host(&url))));
            crate::island::refresh();
        });
    });
}

/// Открыть ярлык.
pub fn launch(item: &LauncherItem) {
    if item.is_group() {
        return;
    }
    let target = wide(&item.target);
    let verb = wide("open");
    let dir = Path::new(&item.target).parent().map(|p| wide(&p.to_string_lossy()));
    unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            dir.as_ref().filter(|_| !item.is_link()).map(|d| PCWSTR(d.as_ptr())).unwrap_or(PCWSTR::null()),
            SW_SHOWNORMAL,
        );
    }
}

/// Найти ярлык по id (в том числе внутри групп).
pub fn find(items: &[LauncherItem], id: &str) -> Option<LauncherItem> {
    for item in items {
        if item.id == id {
            return Some(item.clone());
        }
        if let Some(found) = find(&item.children, id) {
            return Some(found);
        }
    }
    None
}

pub fn launch_id(id: &str) {
    if let Some(item) = settings::read(|s| find(&s.launcher, id)) {
        launch(&item);
    }
}

/// Имя для ярлыка: имя файла без расширения или адрес сайта.
pub fn default_name(target: &str) -> String {
    if target.contains("://") {
        let h = host(target);
        let h = h.strip_prefix("www.").unwrap_or(&h).to_string();
        return if h.is_empty() { target.to_string() } else { h };
    }
    let p = Path::new(target);
    p.file_stem().or_else(|| p.file_name()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| target.to_string())
}

/// Перетащили файлы на остров.
pub fn add_paths(paths: Vec<PathBuf>) {
    settings::update(|s| {
        for p in paths {
            let target = p.to_string_lossy().into_owned();
            if s.launcher.iter().any(|i| i.target.eq_ignore_ascii_case(&target)) {
                continue;
            }
            s.launcher.push(LauncherItem { id: settings::new_id(), name: default_name(&target), target, ..Default::default() });
        }
    });
}

/// Ссылка: сайт без схемы дополняется https://.
pub fn normalize_link(text: &str) -> String {
    let t = text.trim();
    if t.contains("://") {
        t.to_string()
    } else {
        format!("https://{t}")
    }
}

pub fn add_link(text: &str) {
    let target = normalize_link(text);
    if target.len() <= 8 {
        return;
    }
    settings::update(|s| s.launcher.push(LauncherItem { id: settings::new_id(), name: default_name(&target), target, ..Default::default() }));
}

/// Вынуть ярлык из дерева по id.
fn take(items: &mut Vec<LauncherItem>, id: &str) -> Option<LauncherItem> {
    if let Some(i) = items.iter().position(|x| x.id == id) {
        return Some(items.remove(i));
    }
    for item in items.iter_mut() {
        if let Some(found) = take(&mut item.children, id) {
            return Some(found);
        }
    }
    None
}

/// Группа из одного ярлыка — уже не группа: распускаем.
fn tidy(items: &mut Vec<LauncherItem>) {
    for i in 0..items.len() {
        if items[i].children.len() == 1 && items[i].target.is_empty() {
            let only = items[i].children.remove(0);
            items[i] = only;
        }
    }
    items.retain(|i| !(i.target.is_empty() && i.children.is_empty()));
}

/// Перетащили один ярлык на другой: они становятся группой (или ярлык добавляется в группу).
pub fn group(dragged: &str, onto: &str) {
    if dragged == onto {
        return;
    }
    settings::update(|s| {
        let Some(item) = take(&mut s.launcher, dragged) else { return };
        let Some(pos) = s.launcher.iter().position(|x| x.id == onto) else {
            s.launcher.push(item);
            return;
        };
        let target = &mut s.launcher[pos];
        if target.is_group() {
            target.children.push(item);
        } else {
            let first = target.clone();
            *target = LauncherItem {
                id: settings::new_id(),
                name: crate::i18n::tr("Группа", "Group").into(),
                target: String::new(),
                icon: String::new(),
                children: vec![first, item],
            };
        }
        tidy(&mut s.launcher);
    });
}

/// Ярлык в конец ряда (или из группы обратно в ряд).
pub fn move_to_end(id: &str) {
    settings::update(|s| {
        if let Some(item) = take(&mut s.launcher, id) {
            s.launcher.push(item);
        }
        tidy(&mut s.launcher);
    });
}

pub fn remove(id: &str) {
    settings::update(|s| {
        take(&mut s.launcher, id);
        tidy(&mut s.launcher);
        s.hotkeys.retain(|h| !(h.action == settings::HotkeyAction::Launch && h.app.as_deref() == Some(id)));
    });
}

fn find_mut<'a>(items: &'a mut [LauncherItem], id: &str) -> Option<&'a mut LauncherItem> {
    for item in items.iter_mut() {
        if item.id == id {
            return Some(item);
        }
        if let Some(found) = find_mut(&mut item.children, id) {
            return Some(found);
        }
    }
    None
}

pub fn edit(id: &str, f: impl FnOnce(&mut LauncherItem)) {
    settings::update(|s| {
        if let Some(item) = find_mut(&mut s.launcher, id) {
            f(item);
        }
    });
}
