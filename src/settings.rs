//! Настройки в %APPDATA%\OmniFlex\settings.json. Общие для всех потоков; при изменении сохраняются сразу
//! и рассылаются потокам, которым есть до них дело.

use serde::{Deserialize, Serialize};
use std::sync::{OnceLock, RwLock};

use crate::util;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

/// Какие плееры слушает текст песни на рабочем столе.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MusicSourceMode {
    /// Spotify, Яндекс Музыка и YouTube — в том числе открытые в браузере.
    #[default]
    Auto,
    Spotify,
    /// Яндекс Музыка, а также любой плеер в браузере: сайт в медиа-сеансе не виден, поэтому браузер доверяем целиком.
    YandexMusic,
    /// YouTube, а также любой плеер в браузере.
    YouTube,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum HotkeyAction {
    VolumeUp,
    VolumeDown,
    ToggleMute,
    OpenMixer,
    NextDevice,
    /// Включить или выключить микрофон (устройство записи по умолчанию).
    ToggleMic,
    /// Показать или спрятать остров.
    ToggleIsland,
    ScreenshotRegion,
    ScreenshotFull,
    /// Текст с экрана (распознавание).
    ScreenText,
    QrCode,
    ColorPicker,
    /// Начать или остановить запись экрана.
    Record,
    /// Вставить из буфера без форматирования.
    PastePlain,
    /// Голос в текст: первое нажатие начинает запись, второе — распознаёт.
    Voice,
    /// Сквозной режим острова.
    ClickThrough,
    /// Вопрос ИИ.
    AiCommand,
    /// Открыть ярлык лаунчера (его id — в поле app).
    Launch,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum HotkeyTarget {
    #[default]
    Foreground,
    Master,
    App,
}

/// Модификаторы в формате RegisterHotKey.
pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct HotkeyBinding {
    pub action: HotkeyAction,
    #[serde(default)]
    pub target: HotkeyTarget,
    /// Имя exe без расширения (для target = App).
    #[serde(default)]
    pub app: Option<String>,
    #[serde(default)]
    pub modifiers: u32,
    /// Виртуальный код клавиши; 0 — не назначена.
    #[serde(default)]
    pub vk: u32,
}

impl HotkeyBinding {
    pub fn new(action: HotkeyAction) -> Self {
        Self { action, target: HotkeyTarget::Foreground, app: None, modifiers: 0, vk: 0 }
    }

    pub fn has_target(&self) -> bool {
        matches!(self.action, HotkeyAction::VolumeUp | HotkeyAction::VolumeDown | HotkeyAction::ToggleMute)
    }
}

/// Язык программы.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Language {
    /// Русский на русскоязычной Windows, английский на остальных.
    #[default]
    Auto,
    Russian,
    English,
}

/// Как рисуется край-эквалайзер под островом.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum EqStyle {
    /// Столбики с мягкой подсветкой.
    #[default]
    Bars,
    /// Плавная волна.
    Wave,
    /// Столбики из точек.
    Dots,
    /// Светящаяся кромка: яркость и толщина — по громкости.
    Glow,
    /// Столбики цветами радуги.
    Rainbow,
    /// Зеркально: низкие частоты в центре, высокие по краям.
    Mirror,
}

/// Что показывает свёрнутый остров, пока играет музыка.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum MusicPill {
    /// Обычная пилюля, музыку не показывает.
    Off,
    /// Пошире: название трека и тонкая полоса времени.
    Wide,
    /// Мини-плеер: обложка, трек, полоса и кнопки.
    #[default]
    Mini,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RecordFormat {
    #[default]
    Mp4,
    Gif,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum UploadService {
    /// Сервис не выбран: ничего никуда не загружается.
    #[default]
    None,
    Imgur,
    ImgBB,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum VoiceEngine {
    /// Распознавание речи Windows.
    #[default]
    Windows,
    /// Whisper через API из настроек ИИ.
    Api,
}

/// Остров вверху экрана.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct IslandSettings {
    pub enabled: bool,
    /// Прятать, пока на переднем плане игра или полноэкранное окно.
    pub hide_in_games: bool,
    pub show_battery: bool,
    pub show_layout: bool,
    pub show_mic_cam: bool,
    pub show_seconds: bool,
    /// Колёсико мыши над островом меняет общую громкость.
    pub wheel_volume: bool,
    /// Монитор острова: имя устройства, «» — основной.
    pub screen: String,
    /// Тема самого острова.
    pub theme: ThemeMode,
    /// Размер, % (80–200).
    pub size: i32,
    /// Непрозрачность свёрнутого и раскрытого острова, % (20–100).
    pub opacity_collapsed: i32,
    pub opacity_expanded: i32,
    /// Сдвиг вдоль верхнего края монитора, % от его ширины (−45…45).
    pub offset: f32,
    /// Отступ от верхнего края, точки (0 — у самого края).
    pub offset_y: i32,
    /// Показывать остров на скриншотах, в записи и на трансляциях.
    pub show_in_capture: bool,
    /// Сквозной режим: свёрнутый остров пропускает клики и не раскрывается.
    pub click_through: bool,
    /// Автоскрытие: в покое остров уезжает за верхний край и появляется, когда к нему подвели курсор.
    pub auto_hide: bool,
    /// Через сколько секунд после ухода курсора прятаться (1–10).
    pub auto_hide_delay: i32,
    pub music: MusicPill,
    /// Строка песни в такт музыке.
    pub lyrics: bool,
    /// Край-эквалайзер при музыке.
    pub equalizer: bool,
    pub eq_style: EqStyle,
    /// Плеер, который показывает остров (AUMID); «» — тот, что играет.
    pub player: String,
    /// Устройство вывода, громкость которого регулирует остров; «» — по умолчанию в Windows.
    pub output_device: String,
    /// Микрофон, который остров выключает и записывает; «» — по умолчанию в Windows.
    pub mic_device: String,
    /// Уведомления Windows карточкой на острове.
    pub notifications: bool,
    /// Программы, уведомления которых не показывать (имя как в Windows).
    pub notifications_excluded: Vec<String>,
}

impl Default for IslandSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            hide_in_games: true,
            show_battery: true,
            show_layout: true,
            show_mic_cam: true,
            show_seconds: false,
            wheel_volume: true,
            screen: String::new(),
            theme: ThemeMode::Dark,
            size: 100,
            opacity_collapsed: 100,
            opacity_expanded: 100,
            offset: 0.0,
            offset_y: 0,
            show_in_capture: false,
            click_through: false,
            auto_hide: false,
            auto_hide_delay: 2,
            music: MusicPill::Mini,
            lyrics: false,
            equalizer: true,
            eq_style: EqStyle::Bars,
            player: String::new(),
            output_device: String::new(),
            mic_device: String::new(),
            notifications: false,
            notifications_excluded: Vec::new(),
        }
    }
}

/// Скриншоты, запись экрана и прочие инструменты.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct ToolSettings {
    /// Папка скриншотов; «» — Изображения\OmniFlex.
    pub screenshot_dir: String,
    pub save_screenshots: bool,
    pub copy_screenshots: bool,
    /// Папка записей; «» — Видео\OmniFlex.
    pub record_dir: String,
    pub record_fps: i32,
    pub record_high_quality: bool,
    pub record_format: RecordFormat,
    pub upload: UploadService,
    pub imgur_client_id: String,
    pub imgbb_key: String,
    /// История пипетки, свежие первыми: «#RRGGBB».
    pub colors: Vec<String>,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            screenshot_dir: String::new(),
            save_screenshots: true,
            copy_screenshots: true,
            record_dir: String::new(),
            record_fps: 30,
            record_high_quality: false,
            record_format: RecordFormat::Mp4,
            upload: UploadService::None,
            imgur_client_id: String::new(),
            imgbb_key: String::new(),
            colors: Vec::new(),
        }
    }
}

/// Ярлык лаунчера: файл, папка, программа или ссылка; с детьми — группа.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct LauncherItem {
    pub id: String,
    pub name: String,
    /// Путь к файлу или адрес (https://…, steam://run/…).
    pub target: String,
    /// Свой значок: файл .ico/.png/.exe; «» — подтянуть самому.
    pub icon: String,
    pub children: Vec<LauncherItem>,
}

impl LauncherItem {
    pub fn is_group(&self) -> bool {
        !self.children.is_empty()
    }
    pub fn is_link(&self) -> bool {
        !self.is_group() && self.target.contains("://")
    }
}

/// Напоминание. Без дней недели — разовое (на дату date), с днями — повторяется по ним.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct Reminder {
    pub id: String,
    pub text: String,
    /// «ЧЧ:ММ».
    pub time: String,
    /// «ГГГГ-ММ-ДД» для разового.
    pub date: String,
    /// Дни недели: 0 — понедельник … 6 — воскресенье.
    pub days: Vec<u8>,
    pub enabled: bool,
    /// Когда сработало в последний раз: «ГГГГ-ММ-ДД ЧЧ:ММ».
    pub fired: String,
}

/// ИИ через OpenAI-совместимый API.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct AiSettings {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// Модель распознавания речи (Whisper), если сервис её умеет.
    pub transcribe_model: String,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            base_url: "https://text.pollinations.ai/openai".into(),
            api_key: String::new(),
            model: "openai".into(),
            transcribe_model: String::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct VoiceSettings {
    pub engine: VoiceEngine,
    /// Сразу вставить текст в окно, где стоял курсор.
    pub paste: bool,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self { engine: VoiceEngine::Windows, paste: false }
    }
}

pub const ALL_SCREENS: &str = "";
pub const PRIMARY_SCREEN: &str = "primary";

pub const DEFAULT_MARQUEE_SPEED: i32 = 22;
pub const MIN_MARQUEE_SPEED: i32 = 5;
pub const MAX_MARQUEE_SPEED: i32 = 100;
pub const DEFAULT_MARQUEE_FPS: i32 = 60;
pub const MIN_MARQUEE_FPS: i32 = 15;
pub const MAX_MARQUEE_FPS: i32 = 240;

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeMode,
    pub volume_step: i32,
    pub show_osd: bool,
    pub close_to_tray: bool,
    /// Песня и её текст на рабочем столе вместо обоев, пока играет песня.
    pub desktop_lyrics: bool,
    /// Скорость бегущей строки длинного названия на обоях, точек макета 1920×1080 в секунду.
    pub marquee_speed: i32,
    /// Кадров в секунду у бегущей строки.
    pub marquee_fps: i32,
    /// Кнопки «назад», «играть/пауза», «вперёд» на обоях под полосой прогресса.
    pub desktop_controls: bool,
    /// На каких мониторах обои с песней: ALL_SCREENS, PRIMARY_SCREEN или имя устройства.
    pub desktop_screen: String,
    pub source: MusicSourceMode,
    pub island: IslandSettings,
    pub hotkeys: Vec<HotkeyBinding>,
    pub language: Language,
    pub tools: ToolSettings,
    pub launcher: Vec<LauncherItem>,
    pub reminders: Vec<Reminder>,
    pub ai: AiSettings,
    pub voice: VoiceSettings,
    pub version: i32,
}

impl Default for Settings {
    fn default() -> Self {
        let ctrl_alt = MOD_CONTROL | MOD_ALT;
        let key = |action, vk| HotkeyBinding { action, target: HotkeyTarget::Foreground, app: None, modifiers: ctrl_alt, vk };
        Self {
            theme: ThemeMode::System,
            volume_step: 2,
            show_osd: true,
            close_to_tray: true,
            desktop_lyrics: false,
            marquee_speed: DEFAULT_MARQUEE_SPEED,
            marquee_fps: DEFAULT_MARQUEE_FPS,
            desktop_controls: true,
            desktop_screen: ALL_SCREENS.to_string(),
            source: MusicSourceMode::Auto,
            island: IslandSettings::default(),
            hotkeys: vec![
                key(HotkeyAction::VolumeUp, 0x21),   // PageUp
                key(HotkeyAction::VolumeDown, 0x22), // PageDown
                key(HotkeyAction::ToggleMute, 0x23), // End
                key(HotkeyAction::OpenMixer, 0x4D),  // M
                HotkeyBinding { modifiers: MOD_CONTROL | MOD_SHIFT, ..key(HotkeyAction::ScreenshotRegion, 0x53) }, // S
                HotkeyBinding { modifiers: MOD_CONTROL | MOD_SHIFT, ..key(HotkeyAction::PastePlain, 0x56) },       // V
            ],
            language: Language::Auto,
            tools: ToolSettings::default(),
            launcher: Vec::new(),
            reminders: Vec::new(),
            ai: AiSettings::default(),
            voice: VoiceSettings::default(),
            version: CURRENT_VERSION,
        }
    }
}

const CURRENT_VERSION: i32 = 1;

static SETTINGS: OnceLock<RwLock<Settings>> = OnceLock::new();
static LISTENER: OnceLock<Box<dyn Fn(&Settings, &Settings) + Send + Sync>> = OnceLock::new();

fn path() -> std::path::PathBuf {
    util::data_dir().join("settings.json")
}

pub fn load() {
    let mut s = std::fs::read_to_string(path())
        .ok()
        // Блокнот и PowerShell пишут UTF-8 с меткой BOM — serde её не понимает
        .and_then(|text| serde_json::from_str::<Settings>(text.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default();
    s.volume_step = s.volume_step.clamp(1, 25);
    s.marquee_speed = s.marquee_speed.clamp(MIN_MARQUEE_SPEED, MAX_MARQUEE_SPEED);
    s.marquee_fps = s.marquee_fps.clamp(MIN_MARQUEE_FPS, MAX_MARQUEE_FPS);
    s.island.size = s.island.size.clamp(80, 200);
    s.island.opacity_collapsed = s.island.opacity_collapsed.clamp(20, 100);
    s.island.opacity_expanded = s.island.opacity_expanded.clamp(20, 100);
    s.island.auto_hide_delay = s.island.auto_hide_delay.clamp(1, 10);
    s.island.offset = s.island.offset.clamp(-45.0, 45.0);
    s.island.offset_y = s.island.offset_y.clamp(0, 2000);
    s.tools.record_fps = if s.tools.record_fps >= 45 { 60 } else { 30 };
    // Одиночная буква без модификаторов сломала бы набор текста во всех программах
    for h in &mut s.hotkeys {
        if h.vk != 0 && h.modifiers == 0 && !crate::hotkeys::is_standalone_key(h.vk) {
            h.vk = 0;
        }
    }
    s.version = CURRENT_VERSION;
    let _ = SETTINGS.set(RwLock::new(s));
}

/// Кого известить об изменении: (старые, новые).
pub fn set_listener(listener: impl Fn(&Settings, &Settings) + Send + Sync + 'static) {
    let _ = LISTENER.set(Box::new(listener));
}

fn lock() -> &'static RwLock<Settings> {
    SETTINGS.get_or_init(|| RwLock::new(Settings::default()))
}

/// Копия текущих настроек.
pub fn get() -> Settings {
    lock().read().map(|s| s.clone()).unwrap_or_default()
}

/// Чтение без копии всего набора.
pub fn read<R>(f: impl FnOnce(&Settings) -> R) -> R {
    match lock().read() {
        Ok(s) => f(&s),
        Err(e) => f(&e.into_inner()),
    }
}

/// Меняет настройки, сохраняет их и извещает потоки, если что-то поменялось.
/// Новый короткий id для ярлыков и напоминаний.
pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("{:x}{:x}", t, N.fetch_add(1, Ordering::Relaxed))
}

pub fn update(f: impl FnOnce(&mut Settings)) {
    let (old, new) = {
        let mut guard = match lock().write() {
            Ok(g) => g,
            Err(e) => e.into_inner(),
        };
        let old = guard.clone();
        f(&mut guard);
        (old, guard.clone())
    };
    if old == new {
        return;
    }
    save(&new);
    if let Some(listener) = LISTENER.get() {
        listener(&old, &new);
    }
}

fn save(s: &Settings) {
    let _ = std::fs::create_dir_all(util::data_dir());
    if let Ok(text) = serde_json::to_string_pretty(s) {
        // Сначала во временный файл: выключение посреди записи не должно оставить пустые настройки
        let tmp = path().with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path());
        }
    }
}
