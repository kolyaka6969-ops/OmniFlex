//! Мышь над кнопками на обоях. Окно обоев лежит под значками рабочего стола, и щелчки достаются Explorer,
//! поэтому смотрим на мышь глобальным хуком, как Lively. Хук живёт в своём потоке и только сверяет точку
//! с готовыми прямоугольниками кнопок. Нажатие на кнопку забираем у Explorer, всё остальное ему.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetAncestor, GetMessageW, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx,
    WindowFromPoint, GA_ROOT, HHOOK, MSG, MSLLHOOKSTRUCT, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_QUIT,
};

use crate::win;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Control {
    Previous,
    PlayPause,
    Next,
    Seek,
}

/// Кнопка на экране в физических пикселях; owner — номер экрана с обоями.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Target {
    pub bounds: RECT,
    pub owner: usize,
    pub control: Control,
}

fn contains(r: &RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

static TARGETS: Mutex<Vec<Target>> = Mutex::new(Vec::new());
static THREAD: AtomicU32 = AtomicU32::new(0);
static HOOK: Mutex<isize> = Mutex::new(0);

struct HookState {
    hover: Option<Target>,
    pressed: Option<Target>,
    fraction: f64,
}

static STATE: Mutex<HookState> = Mutex::new(HookState { hover: None, pressed: None, fraction: 0.0 });

pub fn set_targets(targets: Vec<Target>) {
    if let Ok(mut t) = TARGETS.lock() {
        *t = targets;
    }
}

pub fn enabled() -> bool {
    THREAD.load(Ordering::SeqCst) != 0
}

pub fn set_enabled(on: bool) {
    if on == enabled() {
        return;
    }
    if on {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("OmniFlex desktop pointer".into())
            .spawn(move || unsafe {
                let tid = GetCurrentThreadId();
                let module = GetModuleHandleW(None).unwrap_or_default();
                let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(on_mouse), module, 0);
                let Ok(hook) = hook else {
                    let _ = tx.send(0);
                    return;
                };
                if let Ok(mut h) = HOOK.lock() {
                    *h = hook.0 as isize;
                }
                let _ = tx.send(tid);
                // Хуку нужен цикл сообщений в его потоке
                let mut msg = MSG::default();
                while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {}
                let _ = UnhookWindowsHookEx(hook);
                if let Ok(mut h) = HOOK.lock() {
                    *h = 0;
                }
            })
            .ok();
        let tid = rx.recv().unwrap_or(0);
        THREAD.store(tid, Ordering::SeqCst);
    } else {
        let tid = THREAD.swap(0, Ordering::SeqCst);
        unsafe {
            let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        let had = STATE.lock().map(|mut s| {
            let had = s.hover.is_some() || s.pressed.is_some();
            s.hover = None;
            s.pressed = None;
            had
        });
        if had.unwrap_or(false) {
            super::pointer_event(None, false, 0.0);
        }
    }
}

unsafe extern "system" fn on_mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let msg = wparam.0 as u32;
    if code >= 0 && matches!(msg, WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP) {
        let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        let ours = std::panic::catch_unwind(|| handle(msg, info.pt)).unwrap_or(false);
        if ours {
            return LRESULT(1); // наше нажатие — Explorer его не видит
        }
    }
    let hook = HOOK.lock().map(|h| *h).unwrap_or(0);
    CallNextHookEx(HHOOK(hook as _), code, wparam, lparam)
}

fn fraction(t: &Target, pt: POINT) -> f64 {
    let w = t.bounds.right - t.bounds.left;
    if w <= 0 {
        0.0
    } else {
        ((pt.x - t.bounds.left) as f64 / w as f64).clamp(0.0, 1.0)
    }
}

/// Разбирает событие мыши; true — событие наше и дальше не идёт.
fn handle(msg: u32, pt: POINT) -> bool {
    let Ok(mut st) = STATE.lock() else { return false };
    // Полосу прогресса тянут: она остаётся целью, даже если мышь с неё ушла
    if let Some(seek) = st.pressed.filter(|p| p.control == Control::Seek) {
        let at = fraction(&seek, pt);
        if msg == WM_LBUTTONUP {
            st.pressed = None;
            st.hover = if contains(&seek.bounds, pt.x, pt.y) { Some(seek) } else { None };
            let hover = st.hover;
            drop(st);
            super::pointer_click(seek, at);
            super::pointer_event(hover, false, at);
            return true;
        }
        if msg == WM_MOUSEMOVE && (at - st.fraction).abs() > 0.0005 {
            st.fraction = at;
            drop(st);
            super::pointer_event(Some(seek), true, at);
        }
        return false;
    }

    let targets = TARGETS.lock().map(|t| t.clone()).unwrap_or_default();
    let mut hit = targets.iter().find(|t| contains(&t.bounds, pt.x, pt.y)).copied();
    // Над кнопкой может быть окно — тогда мышь его. Спрашиваем только над кнопками: это дороже
    if hit.is_some() && msg != WM_LBUTTONUP && !over_desktop(pt) {
        hit = None;
    }
    let hover = st.hover;
    let was_pressed = st.pressed.is_some() && st.pressed == hover;
    let mut ours = false;
    let mut click: Option<(Target, f64)> = None;
    if msg == WM_LBUTTONDOWN {
        st.pressed = hit;
        ours = hit.is_some();
    } else if msg == WM_LBUTTONUP {
        // Отпускание после нашего нажатия тоже наше: Explorer нажатия не видел
        ours = st.pressed.is_some();
        if let Some(h) = hit {
            if st.pressed == Some(h) {
                click = Some((h, fraction(&h, pt)));
            }
        }
        st.pressed = None;
    }
    st.hover = hit;
    let pressed = st.pressed.is_some() && st.pressed == hit;
    let frac = hit.map(|h| fraction(&h, pt)).unwrap_or(0.0);
    let notify = hit != hover || pressed != was_pressed || (hit.map(|h| h.control == Control::Seek).unwrap_or(false) && pressed);
    if notify {
        st.fraction = frac;
    }
    drop(st);
    if let Some((t, f)) = click {
        super::pointer_click(t, f);
    }
    if notify {
        super::pointer_event(hit, pressed, frac);
    }
    ours
}

/// Под точкой рабочий стол (значки, слой обоев, наше окно, Wallpaper Engine), а не чьё-то окно.
fn over_desktop(pt: POINT) -> bool {
    unsafe {
        let window = WindowFromPoint(pt);
        if window.0.is_null() {
            return false;
        }
        let root = GetAncestor(window, GA_ROOT);
        let target = if root.0.is_null() { window } else { root };
        let class = win::window_class(target);
        if class == "Progman" || class == "WorkerW" {
            return true;
        }
        let pid = win::window_pid(target);
        pid == win::own_pid() || win::is_wallpaper_process(pid)
    }
}
