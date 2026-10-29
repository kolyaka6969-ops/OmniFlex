//! Остров — пилюля вверху экрана. Свёрнутая показывает часы и значки состояния, во время музыки — трек
//! (пошире или мини-плеер), на миг раскрывается, когда что-то случилось, показывает карточки (скриншот,
//! напоминание, уведомление, вопрос агента), а под курсором раскрывается в панель: плеер, громкость,
//! инструменты, лаунчер, таймер и напоминания, лента. Окно живёт в потоке окон.

mod cards;
mod menu;
mod view;

pub use cards::Card;
use cards::Act;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, MonitorFromPoint, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowThreadProcessId, KillTimer, SetTimer,
    SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE, HWND_TOPMOST, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOWNOACTIVATE, WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WINDOW_EX_STYLE,
    WM_DROPFILES, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONUP, WM_TIMER,
    WS_EX_ACCEPTFILES, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::agents::{Agent, Status};
use crate::audio::{self, Target};
use crate::game::GameDetector;
use crate::gfx::{ease_out_cubic, Rgba, Tween};
use crate::i18n::tr;
use crate::media::player;
use crate::settings::{self, IslandSettings, MusicPill, Settings};
use crate::sysinfo::{self, Battery};
use crate::wnd::{self, Handler};
use crate::{launcher, shell, tools, util, win};

const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_CAPTURECHANGED: u32 = 0x0215;

pub(crate) const TOP: f32 = 8.0;
pub(crate) const PAD: f32 = 26.0;
pub(crate) const COLLAPSED_H: f32 = 34.0;
pub(crate) const ACTIVITY_H: f32 = 42.0;
pub(crate) const MINI_H: f32 = 52.0;
pub(crate) const EXP_W: f32 = 720.0;
pub(crate) const EXP_H: f32 = 340.0;
pub(crate) const CARD_W: f32 = 440.0;
const MORPH: f32 = 0.32;

const TIMER_TICK: usize = 1;
const TIMER_ANIM: usize = 2;
const TIMER_PEEK: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Activity {
    Volume,
    Layout(String),
    Caps(bool),
    Mic(bool),
    MicUse(String),
    CameraUse(String),
    Power(u8, bool),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Tool {
    Region,
    Full,
    Text,
    Qr,
    Picker,
    Plain,
    Record,
    Ai,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Hit {
    None,
    Body,
    Tab(u8),
    Settings,
    Voice,
    Mute,
    Volume,
    Device,
    Mic,
    Prev,
    PlayPause,
    Next,
    Seek,
    Player,
    Tool(Tool),
    Color(usize),
    LaunchAdd,
    Launch(usize),
    GroupItem(usize),
    Preset(u32),
    TimerPause,
    TimerAdd,
    TimerStop,
    QuickReminder,
    AllReminders,
    CardButton(usize),
    CardBody,
    CardClose,
    RecordStop,
    VoiceStop,
    Feed(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Mode {
    Expanded,
    Card,
    Activity,
    Recording,
    Voice,
    Wide,
    Mini,
    Normal,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Countdown {
    pub end: Instant,
    pub total: Duration,
    pub paused: Option<Duration>,
}

impl Countdown {
    pub fn left(&self) -> Duration {
        match self.paused {
            Some(left) => left,
            None => self.end.saturating_duration_since(Instant::now()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Drag {
    None,
    Volume,
    Seek,
    Launch { id: String, x: f32, y: f32, moved: bool },
    /// Перенос острова: где схватили относительно его точки привязки (центр сверху), в пикселях экрана.
    Move { grab: (i32, i32), start: (i32, i32), moved: bool },
}

/// Обложка трека, готовая к рисованию, и её цвет.
pub(crate) struct Cover {
    pub key: usize,
    pub image: Rc<tiny_skia::Pixmap>,
    pub accent: Rgba,
}

pub(crate) struct Island {
    hwnd: HWND,
    pub settings: IslandSettings,
    shown: bool,
    pub scale: f32,
    /// Центр по горизонтали и верх окна на экране.
    origin: (i32, i32),
    pub w: Tween,
    pub h: Tween,
    pub content: Tween,
    pub expand: Tween,
    pub expanded: bool,
    pub tab: u8,
    hover_since: Option<Instant>,
    /// Курсор вошёл в остров и ещё не раскрыл его: наведение раскроет.
    want_expand: bool,
    /// Прошлый тик видел отпущенную кнопку мыши посреди перетаскивания.
    release_seen: bool,
    leave_at: Option<Instant>,
    inside: bool,
    tracking: bool,
    drop_open: bool,
    pub activity: Option<(Activity, Instant)>,
    pub cards: VecDeque<Card>,
    card_since: Option<Instant>,
    card_sounds: u32,
    pub feed: VecDeque<(String, Card)>,
    pub hover: Hit,
    pub hits: Vec<(f32, f32, f32, f32, Hit)>,
    drag: Drag,
    pub drag_value: f32,
    pub layout: String,
    layout_window: isize,
    pub caps: bool,
    pub battery: Option<Battery>,
    pub mic_users: Vec<String>,
    pub cam_users: Vec<String>,
    pub countdown: Option<Countdown>,
    game: GameDetector,
    hidden_for_game: bool,
    ticks: u32,
    clock: String,
    pub player: player::State,
    pub cover: Option<Cover>,
    last_playing: Option<Instant>,
    pub agents: Vec<Agent>,
    pub open_group: Option<String>,
    anim_running: bool,
    last_mode: Mode,
    /// Где пилюля в буфере окна (логические точки) и где окно на экране.
    pub pill_left: f32,
    win_pos: (i32, i32),
    transparent: bool,
    /// Автоскрытие: 1 — остров на месте, 0 — уехал за верхний край.
    pub peek: Tween,
    auto_hidden: bool,
    hidden_drawn: bool,
    near_since: Option<Instant>,
    away_since: Option<Instant>,
}

thread_local! {
    static ISLAND: RefCell<Option<Rc<RefCell<Island>>>> = const { RefCell::new(None) };
}

fn get() -> Option<Rc<RefCell<Island>>> {
    ISLAND.with(|p| p.borrow().clone())
}

fn with<R>(f: impl FnOnce(&mut Island) -> R) -> Option<R> {
    let island = get()?;
    let mut p = island.try_borrow_mut().ok()?;
    Some(f(&mut p))
}

fn create() -> Option<Rc<RefCell<Island>>> {
    if let Some(p) = get() {
        return Some(p);
    }
    let hwnd = wnd::create(
        "OmniFlexIsland",
        "OmniFlex Island",
        WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_ACCEPTFILES,
        WS_POPUP,
        None,
        (0, 0, 1, 1),
        Rc::new(RefCell::new(IslandHandler)),
    )?;
    unsafe {
        DragAcceptFiles(hwnd, true);
    }
    let island = Rc::new(RefCell::new(Island {
        hwnd,
        settings: IslandSettings::default(),
        shown: false,
        scale: 1.0,
        origin: (0, 0),
        w: Tween::new(160.0),
        h: Tween::new(COLLAPSED_H),
        content: Tween::new(1.0),
        expand: Tween::new(0.0),
        expanded: false,
        tab: 0,
        hover_since: None,
        want_expand: false,
        release_seen: false,
        leave_at: None,
        inside: false,
        tracking: false,
        drop_open: false,
        activity: None,
        cards: VecDeque::new(),
        card_since: None,
        card_sounds: 0,
        feed: VecDeque::new(),
        hover: Hit::None,
        hits: Vec::new(),
        drag: Drag::None,
        drag_value: 0.0,
        layout: sysinfo::keyboard_layout(),
        layout_window: unsafe { GetForegroundWindow().0 as isize },
        caps: caps_lock_now(),
        battery: sysinfo::battery(),
        mic_users: Vec::new(),
        cam_users: Vec::new(),
        countdown: None,
        game: GameDetector::new(),
        hidden_for_game: false,
        ticks: 0,
        clock: String::new(),
        player: player::state(),
        cover: None,
        last_playing: None,
        agents: Vec::new(),
        open_group: None,
        anim_running: false,
        last_mode: Mode::Normal,
        pill_left: 0.0,
        win_pos: (0, 0),
        transparent: false,
        peek: Tween::new(1.0),
        auto_hidden: false,
        hidden_drawn: false,
        near_since: None,
        away_since: None,
    }));
    ISLAND.with(|p| *p.borrow_mut() = Some(island.clone()));
    Some(island)
}

/// Caps Lock глазами активного окна: состояние ввода у каждой очереди своё, поэтому на миг подключаемся к ней.
fn caps_lock_now() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let me = GetCurrentThreadId();
        if fg_thread != 0 && fg_thread != me && AttachThreadInput(me, fg_thread, true).as_bool() {
            let on = GetKeyState(0x14) & 1 != 0;
            let _ = AttachThreadInput(me, fg_thread, false);
            on
        } else {
            sysinfo::caps_lock()
        }
    }
}

// ---------- Команды из других модулей (поток окон) ----------

pub fn apply_settings(s: &Settings) {
    if !s.island.enabled {
        with(|p| {
            p.settings = s.island.clone();
            p.set_shown(false);
            unsafe {
                let _ = KillTimer(p.hwnd, TIMER_TICK);
                let _ = KillTimer(p.hwnd, TIMER_PEEK);
            }
        });
        return;
    }
    let Some(island) = create() else { return };
    let Ok(mut p) = island.try_borrow_mut() else { return };
    p.settings = s.island.clone();
    p.place();
    unsafe {
        let affinity = if p.settings.show_in_capture { WDA_NONE } else { WDA_EXCLUDEFROMCAPTURE };
        let _ = SetWindowDisplayAffinity(p.hwnd, affinity);
        SetTimer(p.hwnd, TIMER_TICK, 250, None);
        if p.settings.auto_hide {
            SetTimer(p.hwnd, TIMER_PEEK, 50, None);
        } else {
            let _ = KillTimer(p.hwnd, TIMER_PEEK);
        }
    }
    if !p.settings.auto_hide {
        p.set_hidden(false);
    }
    p.tick(true);
    let hide = p.settings.hide_in_games && p.game.is_game_in_front();
    p.hidden_for_game = hide;
    p.set_shown(!hide);
    p.retarget();
    p.render();
}

pub fn mixer_changed() {
    with(|p| {
        if p.shown && p.drag == Drag::None {
            p.render();
        }
    });
}

/// Громкость поменяли не на острове (колёсико, горячие клавиши) — показать её на миг.
pub fn volume_changed() {
    with(|p| p.activity(Activity::Volume));
}

pub fn mic_toggled(muted: bool) {
    with(|p| p.activity(Activity::Mic(muted)));
}

/// Клавишу отпустили (сырой ввод): проверить Caps Lock и раскладку сразу, не дожидаясь опроса.
pub fn key_released(vk: u32) {
    with(|p| {
        if !p.shown {
            return;
        }
        if vk == 0x14 {
            let caps = caps_lock_now();
            if caps != p.caps {
                p.caps = caps;
                p.activity(Activity::Caps(caps));
            }
        }
        if matches!(vk, 0x10 | 0x11 | 0x12 | 0x20 | 0x5B | 0x5C | 0xA0..=0xA5) {
            p.check_layout();
        }
    });
}

/// Плеер сменил трек, позицию или состояние.
pub fn player_changed() {
    with(|p| {
        p.player = player::state();
        p.update_cover();
        if p.player.track.as_ref().map(|t| t.playing).unwrap_or(false) {
            p.last_playing = Some(Instant::now());
        }
        p.retarget();
        p.render();
    });
}

/// Карточка на острове. Новая встаёт в очередь за текущей; «занятая» заменяется результатом.
pub fn card(card: Card) {
    with(|p| p.push_card(card));
}

pub fn recording_changed() {
    with(|p| {
        p.retarget();
        p.render();
    });
}

pub fn voice_changed() {
    recording_changed();
}

pub fn refresh() {
    recording_changed();
}

pub fn agent_update(name: String, status: Option<Status>, text: String) {
    with(|p| {
        let old = p.agents.iter().find(|a| a.name == name).map(|a| a.text.clone()).unwrap_or_default();
        p.agents.retain(|a| a.name != name);
        if let Some(status) = status {
            let text = if text.is_empty() { old } else { text };
            p.agents.insert(0, Agent { name, status, text, at: Instant::now() });
        }
        p.retarget();
        p.render();
    });
}

/// Агент перестал ждать ответа (ответили в терминале или вышло время) — убрать его вопрос.
pub fn agent_card_closed(id: u64) {
    with(|p| {
        let before = p.cards.len();
        let first = p.cards.front().and_then(|c| c.agent) == Some(id);
        p.cards.retain(|c| c.agent != Some(id));
        if p.cards.len() != before {
            if first {
                p.card_since = None;
                p.next_card();
            }
            p.retarget();
            p.render();
        }
    });
}

struct IslandHandler;

impl Handler for IslandHandler {
    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_TIMER => {
                with(|p| match wparam.0 {
                    TIMER_TICK => p.tick(false),
                    TIMER_ANIM => p.animate(),
                    TIMER_PEEK => p.check_peek(),
                    _ => {}
                });
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE | WM_MOUSELEAVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_MOUSEWHEEL => {
                let action = with(|p| p.mouse(hwnd, msg, wparam, lparam)).flatten();
                if let Some(a) = action {
                    run_later(a);
                }
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED => {
                // Мышь отобрали посреди перетаскивания (меню, другое окно) — перетаскивание закончено
                with(|p| p.finish_drag());
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                let (px, py) = wnd::point_from_lparam(lparam);
                let hit = with(|p| {
                    let (x, y) = p.to_pill(px, py);
                    p.hit(x, y)
                })
                .unwrap_or(Hit::None);
                match hit {
                    Hit::Launch(i) => launcher_menu(hwnd, top_item(i)),
                    Hit::GroupItem(i) => launcher_menu(hwnd, group_item(i)),
                    _ => context_menu(hwnd),
                }
                Some(LRESULT(0))
            }
            WM_DROPFILES => {
                let drop = HDROP(wparam.0 as _);
                let mut paths = Vec::new();
                unsafe {
                    let count = DragQueryFileW(drop, u32::MAX, None);
                    for i in 0..count {
                        let mut buf = vec![0u16; 1024];
                        let len = DragQueryFileW(drop, i, Some(&mut buf));
                        paths.push(std::path::PathBuf::from(String::from_utf16_lossy(&buf[..len as usize])));
                    }
                    DragFinish(drop);
                }
                launcher::add_paths(paths);
                with(|p| p.drop_open = false);
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

fn top_item(i: usize) -> Option<settings::LauncherItem> {
    settings::read(|s| s.launcher.get(i).cloned())
}

fn group_item(i: usize) -> Option<settings::LauncherItem> {
    let group = with(|p| p.open_group.clone()).flatten()?;
    settings::read(|s| s.launcher.iter().find(|x| x.id == group).and_then(|g| g.children.get(i).cloned()))
}

/// Действия, которые открывают окна или меню: после выхода из обработчика мыши, чтобы не держать остров занятым.
#[derive(Clone, Debug)]
enum Later {
    Settings,
    Tool(Tool),
    Act(Act),
    DeviceMenu,
    PlayerMenu,
    AddLink,
    QuickReminder,
    AllReminders,
    Voice,
}

fn run_later(a: Later) {
    shell::post(move || match a {
        Later::Settings => crate::ui::show_main(crate::ui::Page::Island),
        Later::Tool(t) => {
            with(|p| p.collapse());
            match t {
                Tool::Region => tools::screenshot_region(),
                Tool::Full => tools::screenshot_full(),
                Tool::Text => tools::screen_text(),
                Tool::Qr => tools::qr_code(),
                Tool::Picker => tools::color_picker(),
                Tool::Plain => tools::paste_plain(),
                Tool::Record => tools::toggle_record(),
                Tool::Ai => crate::ai::command(),
            }
        }
        Later::Act(act) => do_act(act),
        Later::DeviceMenu => device_menu(),
        Later::PlayerMenu => player_menu(),
        Later::AddLink => crate::prompt::ask(
            tr("Новая ссылка", "New link"),
            tr("Сайт или команда программы: youtube.com, steam://run/730", "Website or app command: youtube.com, steam://run/730"),
            "",
            |text| launcher::add_link(&text),
        ),
        Later::QuickReminder => crate::prompt::ask(
            tr("Напомнить через 10 минут", "Remind me in 10 minutes"),
            tr("О чём напомнить?", "What to remind about?"),
            "",
            |text| crate::reminders::add_in(10, &text),
        ),
        Later::AllReminders => crate::ui::show_main(crate::ui::Page::Reminders),
        Later::Voice => crate::voice::toggle(),
    });
}

fn do_act(act: Act) {
    match act {
        Act::ReminderDone | Act::TimerDone => {}
        Act::Copy(text) => {
            tools::clipboard::set_text(&text);
        }
        Act::OpenUrl(url) => open(&url),
        Act::OpenPath(p) => open(&p.to_string_lossy()),
        Act::ShowInFolder(p) => {
            let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{}", p.to_string_lossy())).spawn();
        }
        Act::Upload(p) => tools::upload(p),
        Act::ShowText(title, text) => crate::prompt::show_text(&title, &text),
        Act::Snooze(id, text, minutes) => crate::reminders::snooze(&id, &text, minutes),
        Act::Agent(id, answer) => crate::agents::reply(id, answer),
        Act::OpenApp(aumid) => crate::notify::open_app(&aumid),
    }
}

fn open(target: &str) {
    let w = util::wide(target);
    let verb = util::wide("open");
    unsafe {
        windows::Win32::UI::Shell::ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(w.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        );
    }
}

fn island_hwnd() -> HWND {
    with(|p| p.hwnd).unwrap_or_default()
}

/// Меню у курсора в стиле острова; возвращает выбранный номер (0 — ничего).
fn popup(items: &[(usize, String, bool, bool)]) -> usize {
    let scale = with(|p| p.scale).unwrap_or(1.0);
    menu::popup(items, scale)
}

fn item(id: usize, text: &str) -> (usize, String, bool, bool) {
    (id, text.to_string(), false, true)
}

fn context_menu(_hwnd: HWND) {
    let click_through = settings::read(|s| s.island.click_through);
    let cmd = popup(&[
        item(1, tr("Микшер громкости", "Volume mixer")),
        item(2, tr("Настройки острова", "Island settings")),
        (3, tr("Сквозной режим", "Click-through mode").into(), click_through, true),
        item(5, tr("Вернуть на место и обычный размер", "Reset position and size")),
        (0, String::new(), false, true),
        item(4, tr("Спрятать остров", "Hide island")),
    ]);
    match cmd {
        1 => crate::flyout::show(),
        2 => crate::ui::show_main(crate::ui::Page::Island),
        3 => settings::update(|s| s.island.click_through = !s.island.click_through),
        4 => settings::update(|s| s.island.enabled = false),
        5 => settings::update(|s| {
            s.island.screen.clear();
            s.island.offset = 0.0;
            s.island.offset_y = 0;
            s.island.size = 100;
        }),
        _ => {}
    }
}

fn device_menu() {
    let snap = audio::snapshot();
    let items: Vec<(usize, String, bool, bool)> =
        snap.devices.iter().enumerate().map(|(i, d)| (i + 1, d.name.clone(), d.id == snap.device_id, true)).collect();
    let cmd = popup(&items);
    if cmd > 0 {
        if let Some(d) = snap.devices.get(cmd - 1) {
            let id = d.id.clone();
            // Своё устройство острова выбрано — меняем его, иначе устройство Windows по умолчанию
            if settings::read(|s| !s.island.output_device.is_empty()) {
                settings::update(|s| s.island.output_device = id.clone());
                shell::refresh_mixer();
            } else {
                shell::set_device(id);
            }
        }
    }
}

fn player_menu() {
    let state = player::state();
    let chosen = settings::read(|s| s.island.player.clone());
    let mut items = vec![(1, tr("Тот, что играет", "Whichever is playing").to_string(), chosen.is_empty(), true), (0, String::new(), false, true)];
    for (i, pl) in state.players.iter().enumerate() {
        let label = if pl.playing { format!("{}  ▶", pl.app) } else { pl.app.clone() };
        items.push((i + 2, label, !chosen.is_empty() && pl.aumid.eq_ignore_ascii_case(&chosen), true));
    }
    let cmd = popup(&items);
    let value = match cmd {
        0 => return,
        1 => String::new(),
        n => match state.players.get(n - 2) {
            Some(pl) => pl.aumid.clone(),
            None => return,
        },
    };
    settings::update(|s| s.island.player = value);
    player::command(player::Command::Reload);
}

fn launcher_menu(_hwnd: HWND, item_: Option<settings::LauncherItem>) {
    let Some(it) = item_ else { return };
    let in_group = settings::read(|s| !s.launcher.iter().any(|x| x.id == it.id));
    let mut items = vec![item(1, tr("Открыть", "Open")), item(2, tr("Переименовать…", "Rename…"))];
    if it.is_link() {
        items.push(item(3, tr("Изменить ссылку…", "Edit link…")));
    }
    items.push(item(4, tr("Свой значок…", "Custom icon…")));
    if !it.icon.is_empty() {
        items.push(item(5, tr("Значок по умолчанию", "Default icon")));
    }
    if !it.is_group() {
        items.push(item(6, tr("Горячая клавиша…", "Hotkey…")));
    }
    if in_group {
        items.push(item(7, tr("Убрать из группы", "Remove from group")));
    }
    items.push((0, String::new(), false, true));
    items.push(item(8, tr("Удалить", "Delete")));
    let id = it.id.clone();
    match popup(&items) {
        1 => launcher::launch(&it),
        2 => crate::prompt::ask(tr("Имя ярлыка", "Shortcut name"), "", &it.name, move |name| launcher::edit(&id, |x| x.name = name)),
        3 => {
            let old = it.clone();
            crate::prompt::ask(tr("Ссылка", "Link"), "https://…", &it.target, move |text| {
                launcher::forget(&old);
                let target = launcher::normalize_link(&text);
                launcher::edit(&id, |x| x.target = target);
            })
        }
        4 => {
            if let Some(path) = pick_icon_file() {
                launcher::forget(&it);
                launcher::edit(&id, |x| x.icon = path);
            }
        }
        5 => {
            launcher::forget(&it);
            launcher::edit(&id, |x| x.icon.clear());
        }
        6 => {
            settings::update(|s| {
                let mut b = settings::HotkeyBinding::new(settings::HotkeyAction::Launch);
                b.app = Some(id.clone());
                s.hotkeys.push(b);
            });
            crate::ui::show_main(crate::ui::Page::Hotkeys);
        }
        7 => launcher::move_to_end(&id),
        8 => launcher::remove(&id),
        _ => {}
    }
}

/// Выбор файла значка.
fn pick_icon_file() -> Option<String> {
    use windows::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OFN_FILEMUSTEXIST, OPENFILENAMEW};
    let mut buf = vec![0u16; 1024];
    let filter: Vec<u16> = "Значки и картинки\0*.ico;*.png;*.jpg;*.exe;*.dll\0Все файлы\0*.*\0\0".encode_utf16().collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: island_hwnd(),
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: windows::core::PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        Flags: OFN_FILEMUSTEXIST,
        ..Default::default()
    };
    unsafe {
        if GetOpenFileNameW(&mut ofn).as_bool() {
            Some(util::from_wide(&buf))
        } else {
            None
        }
    }
}

impl Island {
    fn set_shown(&mut self, shown: bool) {
        if self.shown == shown {
            return;
        }
        self.shown = shown;
        unsafe {
            if shown {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                let _ = SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            } else {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }
    }

    /// Где остров: выбранный монитор, сдвиг вдоль верхнего края; с панелью задач вверху — под ней.
    fn place(&mut self) {
        let monitor = win::monitor_by_device(&self.settings.screen);
        self.scale = win::monitor_scale(&monitor) * self.settings.size.clamp(80, 200) as f32 / 100.0;
        let b = monitor.bounds;
        let width = b.right - b.left;
        let center = b.left + width / 2 + (width as f32 * self.settings.offset.clamp(-45.0, 45.0) / 100.0).round() as i32;
        let top = if monitor.work.top > b.top { monitor.work.top } else { b.top };
        let below = (self.settings.offset_y.max(0) as f32 * self.scale).round() as i32;
        // Ниже рабочей области не уезжает
        let top = (top + below).min(monitor.work.bottom - ((TOP + COLLAPSED_H) * self.scale) as i32).max(top);
        self.origin = (center, top);
    }

    fn activity(&mut self, activity: Activity) {
        if !self.shown {
            return;
        }
        let duration = match activity {
            Activity::Volume | Activity::Caps(_) | Activity::Mic(_) => 1.6,
            Activity::Layout(_) => 1.3,
            Activity::MicUse(_) | Activity::CameraUse(_) | Activity::Power(..) => 3.0,
        };
        self.activity = Some((activity, Instant::now() + Duration::from_secs_f32(duration)));
        self.retarget();
        self.render();
    }

    fn push_card(&mut self, card: Card) {
        // Результат заменяет «Распознаю…» и подобные
        if self.cards.front().map(|c| c.busy).unwrap_or(false) {
            self.cards.pop_front();
            self.card_since = None;
        }
        if card.feed {
            let time = chrono::Local::now().format("%H:%M").to_string();
            self.feed.push_front((time, card.clone()));
            self.feed.truncate(12);
        }
        // Уведомления и ответы не копятся бесконечно: держим последние
        if self.cards.len() > 6 {
            if let Some(i) = self.cards.iter().position(|c| !c.sticky) {
                self.cards.remove(i);
            }
        }
        self.cards.push_back(card);
        if self.cards.len() == 1 {
            self.next_card();
        }
        self.retarget();
        self.render();
    }

    /// Показать первую карточку очереди.
    fn next_card(&mut self) {
        self.card_since = None;
        self.card_sounds = 0;
        if let Some(c) = self.cards.front() {
            self.card_since = Some(Instant::now());
            if c.sound {
                crate::reminders::play_sound();
                self.card_sounds = 1;
            }
            // Карточка важнее панели: панель сворачивается
            self.expanded = false;
        }
    }

    fn close_card(&mut self) {
        self.cards.pop_front();
        self.next_card();
        self.retarget();
        self.render();
    }

    pub fn track_visible(&self) -> Option<&player::Track> {
        let track = self.player.track.as_ref()?;
        if track.title.trim().is_empty() {
            return None;
        }
        // На паузе мини-плеер держится пару минут: можно снова включить
        let recent = self.last_playing.map(|t| t.elapsed() < Duration::from_secs(120)).unwrap_or(false);
        if track.playing || recent {
            Some(track)
        } else {
            None
        }
    }

    pub fn mode(&self) -> Mode {
        if self.expanded {
            return Mode::Expanded;
        }
        if !self.cards.is_empty() {
            return Mode::Card;
        }
        if self.activity.is_some() {
            return Mode::Activity;
        }
        if tools::record::is_recording() {
            return Mode::Recording;
        }
        if crate::voice::listening_since().is_some() {
            return Mode::Voice;
        }
        if self.countdown.is_none() && self.track_visible().is_some() {
            match self.settings.music {
                MusicPill::Wide => return Mode::Wide,
                MusicPill::Mini => return Mode::Mini,
                MusicPill::Off => {}
            }
        }
        Mode::Normal
    }

    /// Размер, к которому сейчас стремится пилюля.
    pub fn target_size(&self) -> (f32, f32) {
        match self.mode() {
            Mode::Expanded => (EXP_W, EXP_H),
            Mode::Card => (CARD_W, self.cards.front().map(view::card_height).unwrap_or(100.0)),
            Mode::Activity => {
                let w = match &self.activity {
                    Some((Activity::Volume, _)) => 300.0,
                    Some((Activity::MicUse(name), _)) | Some((Activity::CameraUse(name), _)) => {
                        (crate::gfx::text::measure(name, crate::gfx::Weight::Semibold, 13.0) + 130.0).clamp(240.0, 380.0)
                    }
                    _ => 250.0,
                };
                (w, ACTIVITY_H)
            }
            Mode::Recording | Mode::Voice => (230.0, COLLAPSED_H),
            Mode::Wide => (420.0, COLLAPSED_H),
            Mode::Mini => (400.0, MINI_H),
            Mode::Normal => (view::collapsed_width(self), COLLAPSED_H),
        }
    }

    pub fn dragging_volume(&self) -> bool {
        self.drag == Drag::Volume
    }

    pub fn dragging_seek(&self) -> bool {
        self.drag == Drag::Seek
    }

    pub fn clock_text(&self) -> String {
        let now = chrono::Local::now();
        if self.settings.show_seconds {
            now.format("%H:%M:%S").to_string()
        } else {
            now.format("%H:%M").to_string()
        }
    }

    pub fn retarget(&mut self) {
        let (w, h) = self.target_size();
        let mode = self.mode();
        self.w.animate(w, MORPH, ease_out_cubic);
        self.h.animate(h, MORPH, ease_out_cubic);
        self.expand.animate(if self.expanded { 1.0 } else { 0.0 }, MORPH, ease_out_cubic);
        if mode != self.last_mode {
            // Содержимое меняется — старое гаснет, новое проявляется
            self.last_mode = mode;
            self.content.set(0.0);
            self.content.animate(1.0, MORPH, ease_out_cubic);
        }
        // Что-то случилось — спрятанный остров показывается сразу
        if !self.idle() {
            self.set_hidden(false);
        }
        self.update_transparency();
        self.start_anim();
    }

    /// Сквозной режим: свёрнутый остров пропускает мышь. Карточки и вопросы — всегда кликабельны.
    fn update_transparency(&mut self) {
        let mode = self.mode();
        let want = (self.settings.click_through && !matches!(mode, Mode::Expanded | Mode::Card)) || self.auto_hidden;
        if want == self.transparent {
            return;
        }
        self.transparent = want;
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) as u32;
            let ex = if want { ex | WS_EX_TRANSPARENT.0 } else { ex & !WS_EX_TRANSPARENT.0 };
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, WINDOW_EX_STYLE(ex).0 as isize);
        }
    }

    fn start_anim(&mut self) {
        if !self.anim_running {
            self.anim_running = true;
            unsafe {
                SetTimer(self.hwnd, TIMER_ANIM, 16, None);
            }
        }
    }

    /// Нужны ли кадры чаще тика: эквалайзер, пульсация записи, перетаскивание.
    fn continuous(&self) -> bool {
        if self.auto_hidden {
            return false;
        }
        let mode = self.mode();
        let music = matches!(mode, Mode::Wide | Mode::Mini | Mode::Normal | Mode::Expanded)
            && self.settings.equalizer
            && self.player.track.as_ref().map(|t| t.playing).unwrap_or(false)
            && (mode != Mode::Normal || self.track_visible().is_some());
        music || matches!(mode, Mode::Recording | Mode::Voice) || self.drag != Drag::None || self.cards.front().map(|c| c.busy).unwrap_or(false)
    }

    fn animate(&mut self) {
        self.render();
        if !self.w.active() && !self.h.active() && !self.content.active() && !self.expand.active() && !self.peek.active() && !self.continuous() {
            self.anim_running = false;
            crate::eq::stop();
            unsafe {
                let _ = KillTimer(self.hwnd, TIMER_ANIM);
            }
        }
    }

    pub fn collapse(&mut self) {
        if self.expanded {
            self.expanded = false;
            self.open_group = None;
            self.retarget();
            self.render();
        }
    }

    fn tick(&mut self, force: bool) {
        self.ticks = self.ticks.wrapping_add(1);
        let now = Instant::now();
        let mut changed = force;

        if self.ticks % 4 == 0 || force {
            let hide = self.settings.hide_in_games && self.game.is_game_in_front();
            if hide != self.hidden_for_game {
                self.hidden_for_game = hide;
                self.set_shown(!hide);
            }
            crate::reminders::check();
        }
        if !self.shown {
            return;
        }

        // Кнопку мыши отпустили, а окно об этом не узнало: два тика подряд кнопка отпущена — перетаскивание
        // закончено, иначе остров так и считает, что его держат, и не сворачивается
        if self.drag != Drag::None && unsafe { GetAsyncKeyState(0x01) as u16 & 0x8000 == 0 } {
            if self.release_seen {
                self.finish_drag();
                changed = true;
            }
            self.release_seen = true;
        } else {
            self.release_seen = false;
        }
        // Курсор ушёл, а WM_MOUSELEAVE не пришёл (мышь была захвачена) — сворачиваемся всё равно
        if self.expanded && self.drag == Drag::None && !self.drop_open && self.leave_at.is_none() && !self.cursor_over(10.0) {
            self.inside = false;
            self.tracking = false;
            self.want_expand = false;
            self.hover = Hit::None;
            self.leave_at = Some(now + Duration::from_millis(350));
        }

        // Сквозной режим: мышь до острова не доходит, раскрывается он по Ctrl, зажатому над ним
        if self.settings.click_through && !self.expanded && self.cards.is_empty() && unsafe { GetAsyncKeyState(0x11) as u16 & 0x8000 != 0 } && self.cursor_over(0.0) {
            self.expanded = true;
            self.hover_since = None;
            changed = true;
        }

        // Наведение и уход курсора с задержкой: случайный пролёт мыши не раскрывает остров
        if let Some(t) = self.hover_since {
            if !self.expanded && now >= t && self.cards.is_empty() && self.drag == Drag::None {
                self.expanded = true;
                self.want_expand = false;
                self.hover_since = None;
                changed = true;
            }
        }
        if let Some(t) = self.leave_at {
            if self.expanded && now >= t && self.drag == Drag::None {
                self.expanded = false;
                self.open_group = None;
                self.leave_at = None;
                changed = true;
            }
        }
        // Перетаскивание файла из Проводника: остров раскрывается, чтобы бросить его в лаунчер
        self.check_file_drag(&mut changed);

        if let Some((_, until)) = &self.activity {
            if now >= *until {
                self.activity = None;
                changed = true;
            }
        }
        if let (Some(card), Some(since)) = (self.cards.front(), self.card_since) {
            let hovering = self.inside;
            if !card.sticky && !hovering && since.elapsed().as_secs_f32() > card.seconds {
                self.cards.pop_front();
                self.next_card();
                changed = true;
            } else if card.sticky && card.sound && self.card_sounds < 4 && since.elapsed().as_secs() >= 20 * self.card_sounds as u64 {
                crate::reminders::play_sound();
                self.card_sounds += 1;
            }
        }

        let clock = self.clock_text();
        if clock != self.clock {
            self.clock = clock;
            changed = true;
        }

        if let Some(c) = self.countdown {
            if c.paused.is_none() && c.left().is_zero() {
                self.countdown = None;
                self.push_card(Card::timer_done());
            }
            changed = true;
        }
        // Готово у агента — через минуту значок гаснет
        let before = self.agents.len();
        self.agents.retain(|a| !(a.status == Status::Done && a.at.elapsed() > Duration::from_secs(60)));
        changed |= before != self.agents.len();
        if self.track_visible().is_some() || matches!(self.mode(), Mode::Recording | Mode::Voice) {
            changed = true;
        }

        if self.ticks % 2 == 0 {
            self.check_layout();
        }

        if self.ticks % 8 == 0 || force {
            let battery = sysinfo::battery();
            if let (Some(old), Some(new)) = (self.battery, battery) {
                if old.plugged != new.plugged {
                    self.activity(Activity::Power(new.percent, new.plugged));
                }
            }
            if battery != self.battery {
                self.battery = battery;
                changed = true;
            }
            if self.settings.show_mic_cam {
                let mic = sysinfo::devices_in_use("microphone");
                let cam = sysinfo::devices_in_use("webcam");
                if !force {
                    if let Some(new) = mic.iter().find(|m| !self.mic_users.contains(m)) {
                        self.activity(Activity::MicUse(new.clone()));
                    } else if let Some(new) = cam.iter().find(|m| !self.cam_users.contains(m)) {
                        self.activity(Activity::CameraUse(new.clone()));
                    }
                }
                if mic != self.mic_users || cam != self.cam_users {
                    self.mic_users = mic;
                    self.cam_users = cam;
                    changed = true;
                }
            }
            let before = (self.origin, self.scale);
            self.place();
            if before != (self.origin, self.scale) {
                changed = true;
            }
        }

        if changed {
            self.retarget();
            self.render();
        }
    }

    /// Перенос острова мышью: монитор под курсором, сдвиг вдоль края и отступ сверху. Возле центра и у
    /// верхнего края прилипает. В настройки пишется, когда кнопку отпустили.
    fn drag_move(&mut self) {
        let Drag::Move { grab, start, moved } = &mut self.drag else { return };
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        if !*moved && (pt.x - start.0).abs() + (pt.y - start.1).abs() < 5 {
            return;
        }
        *moved = true;
        let grab = *grab;
        let monitor = unsafe { win::monitor_info(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST)) }.unwrap_or_else(win::primary_monitor);
        let b = monitor.bounds;
        let width = (b.right - b.left).max(1) as f32;
        let scale = win::monitor_scale(&monitor) * self.settings.size.clamp(80, 200) as f32 / 100.0;
        let base_top = if monitor.work.top > b.top { monitor.work.top } else { b.top };
        let cx = (pt.x - grab.0) as f32;
        let mut offset = ((cx - (b.left as f32 + width / 2.0)) / width * 100.0).clamp(-45.0, 45.0);
        if offset.abs() < 1.5 {
            offset = 0.0;
        }
        let mut offset_y = ((pt.y - grab.1 - base_top) as f32 / scale).round().clamp(0.0, 2000.0) as i32;
        if offset_y < 12 {
            offset_y = 0;
        }
        self.settings.screen = if monitor.primary { String::new() } else { monitor.device.clone() };
        self.settings.offset = (offset * 10.0).round() / 10.0;
        self.settings.offset_y = offset_y;
        self.place();
        self.render();
    }

    /// Курсор над пилюлей (с запасом в логических точках).
    fn cursor_over(&self, margin: f32) -> bool {
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let (x, y) = self.to_pill(pt.x - self.win_pos.0, pt.y - self.win_pos.1);
        x >= -margin && y >= -margin - TOP && x <= self.w.value() + margin && y <= self.h.value() + margin
    }

    /// Перетаскивание закончилось без отпускания кнопки над островом: позицию сохранить, остальное бросить.
    fn finish_drag(&mut self) {
        if let Drag::Move { moved: true, .. } = std::mem::replace(&mut self.drag, Drag::None) {
            self.save_position();
        }
        self.release_seen = false;
        self.render();
    }

    fn save_position(&self) {
        let (screen, offset, offset_y) = (self.settings.screen.clone(), self.settings.offset, self.settings.offset_y);
        settings::update(|s| {
            s.island.screen = screen;
            s.island.offset = offset;
            s.island.offset_y = offset_y;
        });
    }

    /// Остров в покое: ничего не показывает, курсора и перетаскивания нет — можно прятать.
    fn idle(&self) -> bool {
        self.settings.auto_hide
            && matches!(self.mode(), Mode::Normal | Mode::Wide | Mode::Mini)
            && !self.inside
            && !self.drop_open
            && self.drag == Drag::None
    }

    fn set_hidden(&mut self, hidden: bool) {
        if self.auto_hidden == hidden {
            return;
        }
        self.auto_hidden = hidden;
        self.hidden_drawn = false;
        self.near_since = None;
        self.away_since = None;
        self.peek.animate(if hidden { 0.0 } else { 1.0 }, MORPH, ease_out_cubic);
        self.update_transparency();
        self.start_anim();
    }

    /// Автоскрытие: курсор ушёл от острова — через заданное время он уезжает вверх; курсор подвели к его
    /// месту — возвращается.
    fn check_peek(&mut self) {
        if !self.shown {
            return;
        }
        if !self.idle() {
            self.away_since = None;
            self.set_hidden(false);
            return;
        }
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let s = self.scale;
        let half = (self.w.value() / 2.0 + 40.0) * s;
        let (cx, top) = (self.origin.0 as f32, self.origin.1 as f32);
        let bottom = top + (TOP + self.h.value() + 10.0) * s;
        let (x, y) = (pt.x as f32, pt.y as f32);
        let near = x >= cx - half && x <= cx + half && y >= top - 2.0 && y <= bottom;
        let now = Instant::now();
        if near {
            self.away_since = None;
            if self.auto_hidden && now.duration_since(*self.near_since.get_or_insert(now)) >= Duration::from_millis(120) {
                self.set_hidden(false);
            }
        } else {
            self.near_since = None;
            let delay = Duration::from_secs(self.settings.auto_hide_delay.clamp(1, 10) as u64);
            if !self.auto_hidden && now.duration_since(*self.away_since.get_or_insert(now)) >= delay {
                self.set_hidden(true);
            }
        }
    }

    fn check_file_drag(&mut self, changed: &mut bool) {
        if self.settings.click_through || !self.cards.is_empty() {
            return;
        }
        let pressed = unsafe { GetAsyncKeyState(0x01) as u16 & 0x8000 != 0 };
        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
        }
        let (x, y) = self.to_pill(pt.x - self.win_pos.0, pt.y - self.win_pos.1);
        let (w, h) = (self.w.value(), self.h.value());
        let over = x >= -8.0 && y >= -8.0 && x <= w + 8.0 && y <= h + 8.0;
        if pressed && over && !self.expanded && !self.inside {
            self.expanded = true;
            self.drop_open = true;
            self.tab = 0;
            *changed = true;
        } else if self.drop_open && !pressed && !over {
            self.drop_open = false;
            self.expanded = false;
            *changed = true;
        }
    }

    fn check_layout(&mut self) {
        let window = unsafe { GetForegroundWindow().0 as isize };
        let layout = sysinfo::keyboard_layout();
        if layout != self.layout {
            // У каждого окна своя раскладка: сменилось окно — это не переключение, просто обновляем значок
            let switched = window == self.layout_window;
            self.layout = layout.clone();
            if switched && self.settings.show_layout && self.mode() == Mode::Normal {
                self.activity(Activity::Layout(layout));
            } else {
                self.retarget();
                self.render();
            }
        }
        self.layout_window = window;
    }

    fn update_cover(&mut self) {
        let Some(track) = &self.player.track else {
            self.cover = None;
            return;
        };
        let Some(bytes) = &track.cover else {
            self.cover = None;
            return;
        };
        let key = std::sync::Arc::as_ptr(bytes) as usize;
        if self.cover.as_ref().map(|c| c.key) == Some(key) {
            return;
        }
        let Some(image) = crate::gfx::decode_image(bytes, 192) else {
            self.cover = None;
            return;
        };
        let accent = view::vivid_color(&image);
        self.cover = Some(Cover { key, image: Rc::new(image), accent });
    }

    // ---------- Мышь ----------

    /// Точка окна в логических координатах пилюли (0,0 — её левый верхний угол).
    pub fn to_pill(&self, x: i32, y: i32) -> (f32, f32) {
        let s = self.scale;
        (x as f32 / s - self.pill_left, y as f32 / s - TOP)
    }

    pub fn hit(&self, x: f32, y: f32) -> Hit {
        let (w, h) = (self.w.value(), self.h.value());
        if x < 0.0 || y < 0.0 || x > w || y > h {
            return Hit::None;
        }
        if self.auto_hidden || self.peek.active() {
            return Hit::None;
        }
        // Пока пилюля заметно меняет размер, кнопки ещё не на местах; мелкая подстройка щелчкам не мешает
        let (tw, th) = self.target_size();
        if (w - tw).abs() > 6.0 || (h - th).abs() > 4.0 {
            return Hit::Body;
        }
        // Последние нарисованные — сверху
        for (hx, hy, hw, hh, hit) in self.hits.iter().rev() {
            if x >= *hx && x < hx + hw && y >= *hy && y < hy + hh {
                return *hit;
            }
        }
        if self.mode() == Mode::Card {
            return Hit::CardBody;
        }
        Hit::Body
    }

    fn rect_of(&self, hit: Hit) -> Option<(f32, f32, f32, f32)> {
        self.hits.iter().find(|r| r.4 == hit).map(|r| (r.0, r.1, r.2, r.3))
    }

    fn fraction(&self, hit: Hit, x: f32) -> f32 {
        match self.rect_of(hit) {
            Some((rx, _, rw, _)) => ((x - rx) / rw.max(1.0)).clamp(0.0, 1.0),
            None => 0.0,
        }
    }

    fn mouse(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<Later> {
        let (mut px, mut py) = wnd::point_from_lparam(lparam);
        if msg == WM_MOUSEWHEEL {
            let mut origin = POINT::default();
            unsafe {
                let _ = ClientToScreen(hwnd, &mut origin);
            }
            px -= origin.x;
            py -= origin.y;
        }
        let (x, y) = self.to_pill(px, py);
        match msg {
            WM_MOUSEMOVE => {
                if !self.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut tme);
                    }
                    self.tracking = true;
                }
                let hit_now = self.hit(x, y);
                let over = hit_now != Hit::None;
                if over && !self.inside {
                    self.inside = true;
                    self.want_expand = true;
                }
                // Над кнопками плеера свёрнутый остров не раскрывается — по ним щёлкают
                if over && self.want_expand && !self.expanded && self.cards.is_empty() && self.drag == Drag::None {
                    if matches!(hit_now, Hit::Prev | Hit::PlayPause | Hit::Next) {
                        self.hover_since = None;
                    } else if self.hover_since.is_none() {
                        self.hover_since = Some(Instant::now() + Duration::from_millis(160));
                    }
                }
                if over {
                    self.leave_at = None;
                }
                match &mut self.drag {
                    Drag::Volume => {
                        self.drag_value = (self.fraction(Hit::Volume, x) * 100.0).round();
                        let v = self.drag_value;
                        shell::with_mixer(|m| m.set_volume(&Target::Master, v));
                        shell::after_mixer_change();
                        self.render();
                        return None;
                    }
                    Drag::Seek => {
                        self.drag_value = self.fraction(Hit::Seek, x);
                        self.render();
                        return None;
                    }
                    Drag::Launch { x: x0, y: y0, moved, .. } => {
                        if (x - *x0).abs() + (y - *y0).abs() > 6.0 {
                            *moved = true;
                        }
                    }
                    Drag::Move { .. } => {
                        self.drag_move();
                        return None;
                    }
                    Drag::None => {}
                }
                let hit = self.hit(x, y);
                if hit != self.hover {
                    self.hover = hit;
                    self.render();
                }
                None
            }
            WM_MOUSELEAVE => {
                self.tracking = false;
                self.inside = false;
                self.want_expand = false;
                self.hover_since = None;
                if self.hover != Hit::None {
                    self.hover = Hit::None;
                    self.render();
                }
                if self.expanded && self.drag == Drag::None && !self.drop_open {
                    self.leave_at = Some(Instant::now() + Duration::from_millis(350));
                }
                None
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16;
                // Ctrl + колёсико — размер острова
                if wparam.0 & 0x8 != 0 {
                    let size = (self.settings.size + if delta > 0 { 10 } else { -10 }).clamp(80, 200);
                    if size != self.settings.size {
                        self.settings.size = size;
                        settings::update(|s| s.island.size = size);
                    }
                    return None;
                }
                if !self.settings.wheel_volume {
                    return None;
                }
                let step = settings::read(|s| s.volume_step) as f32;
                let snap = audio::snapshot();
                let v = (snap.master.volume + if delta > 0 { step } else { -step }).clamp(0.0, 100.0);
                shell::with_mixer(|m| {
                    if delta > 0 && snap.master.muted {
                        m.set_mute(&Target::Master, false);
                    }
                    m.set_volume(&Target::Master, v)
                });
                shell::after_mixer_change();
                if !self.expanded && self.cards.is_empty() {
                    self.activity(Activity::Volume);
                } else {
                    self.render();
                }
                None
            }
            WM_LBUTTONDOWN => {
                let hit = self.hit(x, y);
                match hit {
                    Hit::Volume => {
                        self.drag = Drag::Volume;
                        self.drag_value = (self.fraction(Hit::Volume, x) * 100.0).round();
                        let v = self.drag_value;
                        shell::with_mixer(|m| {
                            m.set_mute(&Target::Master, false);
                            m.set_volume(&Target::Master, v)
                        });
                        shell::after_mixer_change();
                    }
                    Hit::Seek => {
                        self.drag = Drag::Seek;
                        self.drag_value = self.fraction(Hit::Seek, x);
                    }
                    Hit::Launch(i) => {
                        if let Some(it) = top_item(i) {
                            self.drag = Drag::Launch { id: it.id, x, y, moved: false };
                        }
                    }
                    // За пустое место остров переносится
                    Hit::Body => {
                        let mut pt = POINT::default();
                        unsafe {
                            let _ = GetCursorPos(&mut pt);
                        }
                        self.hover_since = None;
                        self.drag = Drag::Move { grab: (pt.x - self.origin.0, pt.y - self.origin.1), start: (pt.x, pt.y), moved: false };
                    }
                    _ => return None,
                }
                unsafe {
                    SetCapture(hwnd);
                }
                self.start_anim();
                self.render();
                None
            }
            WM_LBUTTONUP => {
                let drag = std::mem::replace(&mut self.drag, Drag::None);
                if drag != Drag::None {
                    unsafe {
                        let _ = ReleaseCapture();
                    }
                }
                match drag {
                    Drag::Volume => {
                        self.render();
                        return None;
                    }
                    Drag::Move { moved, .. } => {
                        if moved {
                            self.save_position();
                        }
                        self.render();
                        return None;
                    }
                    Drag::Seek => {
                        player::command(player::Command::Seek(self.drag_value as f64));
                        self.render();
                        return None;
                    }
                    Drag::Launch { id, moved: true, .. } => {
                        match self.hit(x, y) {
                            Hit::Launch(j) => {
                                if let Some(target) = top_item(j) {
                                    launcher::group(&id, &target.id);
                                }
                            }
                            Hit::LaunchAdd | Hit::Body => launcher::move_to_end(&id),
                            _ => {}
                        }
                        self.render();
                        return None;
                    }
                    _ => {}
                }
                let hit = self.hit(x, y);
                self.click(hit)
            }
            _ => None,
        }
    }

    fn click(&mut self, hit: Hit) -> Option<Later> {
        let mut later = None;
        match hit {
            Hit::Tab(t) => {
                self.tab = t;
                self.open_group = None;
            }
            Hit::Settings => later = Some(Later::Settings),
            Hit::Voice | Hit::VoiceStop => later = Some(Later::Voice),
            Hit::RecordStop => tools::record::stop(),
            Hit::Mute => {
                let snap = audio::snapshot();
                shell::with_mixer(|m| m.set_mute(&Target::Master, !snap.master.muted));
                shell::after_mixer_change();
            }
            Hit::Mic => {
                if let Some(muted) = shell::with_mixer(|m| m.toggle_mic()).flatten() {
                    shell::after_mixer_change();
                    if !self.expanded {
                        self.activity(Activity::Mic(muted));
                    }
                }
            }
            Hit::Device => later = Some(Later::DeviceMenu),
            Hit::Player => later = Some(Later::PlayerMenu),
            Hit::Prev => player::command(player::Command::Previous),
            Hit::Next => player::command(player::Command::Next),
            Hit::PlayPause => player::command(player::Command::PlayPause),
            Hit::Tool(t) => later = Some(Later::Tool(t)),
            Hit::Color(i) => {
                if let Some(c) = settings::read(|s| s.tools.colors.get(i).cloned()) {
                    tools::clipboard::set_text(&c);
                    self.push_card(Card::text(tr("Цвет скопирован", "Color copied"), c));
                }
            }
            Hit::LaunchAdd => later = Some(Later::AddLink),
            Hit::Launch(i) => {
                if let Some(it) = top_item(i) {
                    if it.is_group() {
                        self.open_group = if self.open_group.as_deref() == Some(&it.id) { None } else { Some(it.id.clone()) };
                    } else {
                        launcher::launch(&it);
                        self.collapse();
                    }
                }
            }
            Hit::GroupItem(i) => {
                if let Some(it) = group_item(i) {
                    launcher::launch(&it);
                    self.collapse();
                }
            }
            Hit::Preset(minutes) => {
                let total = Duration::from_secs(minutes as u64 * 60);
                self.countdown = Some(Countdown { end: Instant::now() + total, total, paused: None });
            }
            Hit::TimerPause => {
                if let Some(c) = &mut self.countdown {
                    match c.paused {
                        Some(left) => {
                            c.end = Instant::now() + left;
                            c.paused = None;
                        }
                        None => c.paused = Some(c.left()),
                    }
                }
            }
            Hit::TimerAdd => {
                if let Some(c) = &mut self.countdown {
                    let minute = Duration::from_secs(60);
                    match &mut c.paused {
                        Some(left) => *left += minute,
                        None => c.end += minute,
                    }
                    c.total += minute;
                }
            }
            Hit::TimerStop => self.countdown = None,
            Hit::QuickReminder => later = Some(Later::QuickReminder),
            Hit::AllReminders => later = Some(Later::AllReminders),
            Hit::CardButton(i) => {
                let act = self.cards.front().and_then(|c| c.buttons.get(i).map(|b| b.1.clone()));
                if let Some(act) = act {
                    if !matches!(act, Act::Copy(_)) || self.cards.front().map(|c| !c.sticky).unwrap_or(true) {
                        self.close_card();
                    }
                    later = Some(Later::Act(act));
                }
            }
            Hit::CardBody => {
                let card = self.cards.front().cloned();
                if let Some(card) = card {
                    // У ждущей карточки щелчок мимо кнопок ничего не решает
                    if card.sticky {
                        return None;
                    }
                    self.close_card();
                    if let Some(act) = card.click {
                        later = Some(Later::Act(act));
                    }
                }
            }
            Hit::CardClose => {
                if let Some(id) = self.cards.front().and_then(|c| c.agent) {
                    crate::agents::reply(id, String::new());
                }
                self.close_card();
            }
            Hit::Feed(i) => {
                if let Some(act) = self.feed.get(i).and_then(|(_, c)| c.click.clone().or_else(|| Some(Act::ShowText(c.title.clone(), c.body.clone())))) {
                    later = Some(Later::Act(act));
                }
            }
            _ => return None,
        }
        self.retarget();
        self.render();
        later
    }

    // ---------- Кадр ----------

    pub fn render(&mut self) {
        if !self.shown {
            return;
        }
        // Спрятанный остров достаточно нарисовать пустым один раз
        if self.auto_hidden && !self.peek.active() {
            if self.hidden_drawn {
                return;
            }
            self.hidden_drawn = true;
        }
        let Some((pm, pill_left, alpha)) = view::frame(self) else { return };
        self.pill_left = pill_left;
        let x = self.origin.0 - pm.width() as i32 / 2;
        let y = self.origin.1;
        self.win_pos = (x, y);
        wnd::update_layered_alpha(self.hwnd, Some((x, y)), &pm, alpha);
    }
}

pub(crate) fn timer_presets() -> [(u32, &'static str); 6] {
    if crate::i18n::is_english() {
        [(1, "1 min"), (3, "3 min"), (5, "5 min"), (10, "10 min"), (15, "15 min"), (30, "30 min")]
    } else {
        [(1, "1 мин"), (3, "3 мин"), (5, "5 мин"), (10, "10 мин"), (15, "15 мин"), (30, "30 мин")]
    }
}

pub(crate) fn format_left(d: Duration) -> String {
    let secs = d.as_secs() + if d.subsec_millis() > 0 { 1 } else { 0 };
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

pub(crate) fn format_time(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// Проверка отрисовки без экрана: `OmniFlex.exe --render-test <папка> [обложка]` сохраняет кадры всех видов острова.
pub fn render_test(dir: &std::path::Path, cover: Option<std::path::PathBuf>) {
    let _ = std::fs::create_dir_all(dir);
    let Some(island) = create() else { return };
    let mut p = island.borrow_mut();
    p.settings = settings::get().island;
    p.scale = 1.25;
    p.clock = p.clock_text();
    let save = |p: &mut Island, name: &str| {
        let (w, h) = p.target_size();
        p.w.set(w);
        p.h.set(h);
        p.content.set(1.0);
        p.expand.set(if p.expanded { 1.0 } else { 0.0 });
        if let Some((pm, _, _)) = view::frame(p) {
            let _ = pm.save_png(dir.join(format!("{name}.png")));
        }
    };
    save(&mut p, "1-normal");
    p.player.track = Some(player::Track {
        aumid: "Spotify.exe".into(),
        app: "Spotify".into(),
        title: "Lithium".into(),
        artist: "Nirvana".into(),
        cover: cover.and_then(|c| std::fs::read(c).ok()).map(std::sync::Arc::new),
        playing: true,
        position: Duration::from_secs(47),
        position_time: Some(std::time::SystemTime::now()),
        duration: Duration::from_secs(255),
    });
    p.player.players = vec![player::PlayerInfo { aumid: "Spotify.exe".into(), app: "Spotify".into(), playing: true }];
    p.update_cover();
    p.settings.music = MusicPill::Off;
    save(&mut p, "1-normal-player");
    p.settings.music = MusicPill::Mini;
    save(&mut p, "2-mini");
    p.settings.music = MusicPill::Wide;
    save(&mut p, "3-wide");
    p.expanded = true;
    for tab in 0..3u8 {
        p.tab = tab;
        save(&mut p, &format!("4-panel-{tab}"));
    }
    p.expanded = false;
    p.cards.push_back(Card::reminder("x", "Позвонить маме"));
    save(&mut p, "5-card-reminder");
    p.cards.clear();
    p.cards.push_back(Card::agent_question(1, "Claude Code: Bash".into(), "cargo build --release".into(), Vec::new()));
    save(&mut p, "6-card-agent");
    p.cards.clear();
    p.cards.push_back(Card::text("Текст скопирован", "Пример распознанного текста с экрана, который не помещается в одну строку и переносится на следующую".into()));
    save(&mut p, "7-card-text");
    let items = [
        item(1, tr("Микшер громкости", "Volume mixer")),
        item(2, tr("Настройки острова", "Island settings")),
        (3, tr("Сквозной режим", "Click-through mode").into(), true, true),
        item(5, tr("Вернуть на место и обычный размер", "Reset position and size")),
        (0, String::new(), false, true),
        (4, tr("Спрятать остров", "Hide island").into(), false, false),
    ];
    let _ = menu::render_test(&items, p.scale).save_png(dir.join("8-menu.png"));
}
