//! Главное окно (egui): микшер, горячие клавиши и настройки. Закрытие прячет его в трей,
//! открыть снова можно из трея, острова или вторым запуском программы.

use std::collections::HashMap;
use std::sync::atomic::{AtomicIsize, AtomicU8, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, FontId, RichText, Vec2};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindow, SW_HIDE, SW_RESTORE, SW_SHOW,
};

use crate::audio::{self, Channel, Target};
use crate::hotkeys;
use crate::i18n::tr;
use crate::settings::{
    self, EqStyle, HotkeyAction, HotkeyBinding, HotkeyTarget, Language, MusicPill, MusicSourceMode, RecordFormat, ThemeMode, UploadService,
    VoiceEngine,
};
use crate::{shell, util, win};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Mixer = 1,
    Hotkeys = 2,
    Settings = 3,
    Island = 4,
    Tools = 5,
    Reminders = 6,
    Ai = 7,
}

fn page_of(n: u8) -> Option<Page> {
    Some(match n {
        1 => Page::Mixer,
        2 => Page::Hotkeys,
        3 => Page::Settings,
        4 => Page::Island,
        5 => Page::Tools,
        6 => Page::Reminders,
        7 => Page::Ai,
        _ => return None,
    })
}

static CTX: OnceLock<egui::Context> = OnceLock::new();
static HWND_MAIN: AtomicIsize = AtomicIsize::new(0);
/// Страница, которую надо открыть при следующем кадре (0 — оставить как есть).
static PENDING_PAGE: AtomicU8 = AtomicU8::new(0);

pub fn request_repaint() {
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

fn hwnd() -> HWND {
    HWND(HWND_MAIN.load(Ordering::SeqCst) as _)
}

/// Показать главное окно на нужной странице (из любого потока).
pub fn show_main(page: Page) {
    PENDING_PAGE.store(page as u8, Ordering::SeqCst);
    let h = hwnd();
    if h.0.is_null() {
        return;
    }
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        } else {
            let _ = ShowWindow(h, SW_SHOW);
        }
        let _ = SetForegroundWindow(h);
    }
    request_repaint();
}

fn hide_main() {
    unsafe {
        let _ = ShowWindow(hwnd(), SW_HIDE);
    }
    shell::MIXER_VIEW_VISIBLE.store(false, Ordering::Relaxed);
}

fn window_visible() -> bool {
    let h = hwnd();
    unsafe { !h.0.is_null() && IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() }
}

pub fn run(start_hidden: bool) {
    let icon = load_icon();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(util::APP_NAME)
        .with_inner_size([940.0, 680.0])
        .with_min_inner_size([760.0, 520.0])
        .with_visible(!start_hidden);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    let result = eframe::run_native(
        util::APP_NAME,
        options,
        Box::new(|cc| {
            use raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = cc.window_handle() {
                if let RawWindowHandle::Win32(w) = handle.as_raw() {
                    HWND_MAIN.store(w.hwnd.get(), Ordering::SeqCst);
                }
            }
            let _ = CTX.set(cc.egui_ctx.clone());
            setup_fonts(&cc.egui_ctx);
            Ok(Box::new(MainApp::new()))
        }),
    );
    if let Err(e) = result {
        util::log_error(format!("окно настроек: {e}"));
    }
}

fn load_icon() -> Option<egui::IconData> {
    let bytes = include_bytes!("../res/app.ico");
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Ico).ok()?.to_rgba8();
    Some(egui::IconData { width: img.width(), height: img.height(), rgba: img.into_raw() })
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let mut add = |name: &str, file: &str, family: FontFamily, first: bool| {
        if let Some(bytes) = crate::gfx::text::font_bytes(file) {
            fonts.font_data.insert(name.into(), FontData::from_owned(bytes));
            let list = fonts.families.entry(family).or_default();
            if first {
                list.insert(0, name.into());
            } else {
                list.push(name.into());
            }
        }
    };
    add("segoe", "segoeui.ttf", FontFamily::Proportional, true);
    add("segoe-symbols", "seguisym.ttf", FontFamily::Proportional, false);
    add("segoe-semibold", "seguisb.ttf", FontFamily::Name("semibold".into()), true);
    add("segoe-symbols2", "seguisym.ttf", FontFamily::Name("semibold".into()), false);
    if crate::gfx::text::font_bytes("SegoeIcons.ttf").is_some() {
        add("icons", "SegoeIcons.ttf", FontFamily::Name("icons".into()), true);
    } else {
        add("icons", "segmdl2.ttf", FontFamily::Name("icons".into()), true);
    }
    ctx.set_fonts(fonts);
}

fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("icons".into()))
}

fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("semibold".into()))
}

struct MainApp {
    page: Page,
    capture: Option<usize>,
    capture_armed: bool,
    textures: HashMap<String, egui::TextureHandle>,
    dark: Option<bool>,
    autostart: bool,
    /// Ползунок, который тянут: значение под мышью важнее запоздалого снимка.
    dragging: Option<(String, f32)>,
    /// Новое напоминание: текст, время, дни.
    new_reminder: (String, String, String),
    agent_error: Option<String>,
}

impl MainApp {
    fn new() -> Self {
        let page = page_of(PENDING_PAGE.swap(0, Ordering::SeqCst)).unwrap_or(Page::Island);
        Self {
            page,
            capture: None,
            capture_armed: false,
            textures: HashMap::new(),
            dark: None,
            autostart: win::autostart_enabled(),
            dragging: None,
            new_reminder: Default::default(),
            agent_error: None,
        }
    }

    fn apply_theme(&mut self, ctx: &egui::Context) {
        let dark = crate::theme::is_dark();
        if self.dark == Some(dark) {
            return;
        }
        self.dark = Some(dark);
        let accent = crate::theme::accent();
        let accent = Color32::from_rgb(accent.r, accent.g, accent.b);
        let mut visuals = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        visuals.selection.bg_fill = accent;
        visuals.hyperlink_color = accent;
        visuals.widgets.active.bg_fill = accent;
        visuals.slider_trailing_fill = true;
        visuals.window_rounding = 8.0.into();
        visuals.panel_fill = if dark { Color32::from_rgb(0x20, 0x20, 0x22) } else { Color32::from_rgb(0xF3, 0xF3, 0xF3) };
        ctx.set_visuals(visuals);
        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.slider_width = 260.0;
        style.spacing.button_padding = Vec2::new(10.0, 5.0);
        style.text_styles.insert(egui::TextStyle::Body, FontId::proportional(14.5));
        style.text_styles.insert(egui::TextStyle::Button, FontId::proportional(14.5));
        style.text_styles.insert(egui::TextStyle::Heading, semibold(24.0));
        style.text_styles.insert(egui::TextStyle::Small, FontId::proportional(12.5));
        ctx.set_style(style);
    }

    fn texture(&mut self, ctx: &egui::Context, channel: &Channel) -> Option<egui::TextureHandle> {
        let icon = channel.icon.as_ref()?;
        if let Some(t) = self.textures.get(&channel.key) {
            return Some(t.clone());
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
        let t = ctx.load_texture(format!("icon-{}", channel.key), image, egui::TextureOptions::LINEAR);
        self.textures.insert(channel.key.clone(), t.clone());
        Some(t)
    }
}

impl eframe::App for MainApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_theme(ctx);
        if let Some(page) = page_of(PENDING_PAGE.swap(0, Ordering::SeqCst)) {
            if self.page == Page::Hotkeys && page != Page::Hotkeys {
                self.stop_capture();
            }
            self.page = page;
        }

        if ctx.input(|i| i.viewport().close_requested()) {
            if settings::read(|s| s.close_to_tray) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.stop_capture();
                hide_main();
            } else {
                crate::exit_app();
            }
        }

        let visible = window_visible();
        let mixer_visible = visible && self.page == Page::Mixer;
        let was = shell::MIXER_VIEW_VISIBLE.swap(mixer_visible, Ordering::Relaxed);
        if mixer_visible && !was {
            shell::refresh_mixer();
        }
        if mixer_visible {
            ctx.request_repaint_after(Duration::from_millis(60));
        }

        egui::SidePanel::left("nav").resizable(false).exact_width(210.0).show(ctx, |ui| {
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.label(RichText::new(util::APP_NAME).font(semibold(20.0)));
            });
            ui.add_space(16.0);
            for (page, glyph, label) in [
                (Page::Island, '\u{E80F}', tr("Остров", "Island")),
                (Page::Mixer, '\u{E767}', tr("Микшер", "Mixer")),
                (Page::Tools, '\u{E7A8}', tr("Инструменты", "Tools")),
                (Page::Reminders, '\u{E823}', tr("Напоминания", "Reminders")),
                (Page::Ai, '\u{E99A}', tr("ИИ и голос", "AI and voice")),
                (Page::Hotkeys, '\u{E765}', tr("Горячие клавиши", "Hotkeys")),
                (Page::Settings, '\u{E713}', tr("Настройки", "Settings")),
            ] {
                // Значки — в один столбец, подписи начинаются с одной линии
                let selected = self.page == page;
                let (rect, response) = ui.allocate_exact_size(egui::vec2(190.0, 36.0), egui::Sense::click());
                let visuals = ui.style().interact_selectable(&response, selected);
                if selected || response.hovered() {
                    ui.painter().rect_filled(rect, visuals.rounding, visuals.weak_bg_fill);
                }
                let color = visuals.text_color();
                let cy = rect.center().y;
                ui.painter().text(egui::pos2(rect.left() + 26.0, cy), egui::Align2::CENTER_CENTER, glyph, icon_font(15.0), color);
                ui.painter().text(egui::pos2(rect.left() + 48.0, cy), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(14.5), color);
                if response.clicked() {
                    if self.page == Page::Hotkeys {
                        self.stop_capture();
                    }
                    self.page = page;
                }
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.add_space(8.0);
                match self.page {
                    Page::Mixer => self.mixer_page(ui, ctx),
                    Page::Hotkeys => self.hotkeys_page(ui, ctx),
                    Page::Settings => self.settings_page(ui),
                    Page::Island => self.island_page(ui),
                    Page::Tools => self.tools_page(ui),
                    Page::Reminders => self.reminders_page(ui),
                    Page::Ai => self.ai_page(ui),
                }
                ui.add_space(16.0);
            });
        });
    }
}


fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let fill = if ui.visuals().dark_mode { Color32::from_rgb(0x2B, 0x2B, 0x2D) } else { Color32::WHITE };
    egui::Frame::none()
        .fill(fill)
        .rounding(8.0)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .inner_margin(egui::Margin::symmetric(16.0, 12.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).font(semibold(15.0)));
}

fn description(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().weak());
}

/// Строка настроек: заголовок с описанием слева, элемент справа.
fn setting_row(ui: &mut egui::Ui, title: &str, desc: &str, control: impl FnOnce(&mut egui::Ui)) {
    card(ui, |ui| {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_max_width((ui.available_width() - 240.0).max(200.0));
                ui.label(RichText::new(title).font(semibold(14.5)));
                if !desc.is_empty() {
                    description(ui, desc);
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
        });
    });
}

fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = Vec2::new(40.0, 20.0);
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(response.id, *on);
    let visuals = ui.style().interact_selectable(&response, *on);
    let radius = rect.height() / 2.0;
    let fill = if *on { ui.visuals().selection.bg_fill } else { Color32::TRANSPARENT };
    ui.painter().rect(rect, radius, fill, visuals.fg_stroke);
    let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
    let knob = if *on { Color32::WHITE } else { visuals.fg_stroke.color };
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), radius * 0.6, knob);
    ui.label(if *on { tr("Вкл.", "On") } else { tr("Выкл.", "Off") });
    response
}

fn toggle_setting(ui: &mut egui::Ui, title: &str, desc: &str, value: bool, set: impl FnOnce(&mut settings::Settings, bool)) {
    let mut v = value;
    let mut changed = false;
    setting_row(ui, title, desc, |ui| {
        changed = toggle(ui, &mut v).changed();
    });
    if changed {
        settings::update(|s| set(s, v));
    }
}

impl MainApp {
    // ---------- Микшер ----------

    fn channel_row(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, channel: &Channel, target: Target, subtitle: Option<&str>) {
        card(ui, |ui| {
            ui.horizontal(|ui| {
                // Значок: щелчок — без звука
                let texture = if channel.muted { None } else { self.texture(ctx, channel) };
                let button = match texture {
                    Some(t) => ui.add(egui::ImageButton::new(egui::Image::new((t.id(), Vec2::splat(24.0)))).frame(false)),
                    None => {
                        let glyph = if channel.muted {
                            '\u{E74F}'
                        } else if target == Target::Master {
                            channel.glyph()
                        } else if channel.is_system {
                            '\u{E7F5}'
                        } else {
                            '\u{E71D}'
                        };
                        ui.add_sized([32.0, 32.0], egui::Button::new(RichText::new(glyph.to_string()).font(icon_font(18.0))).frame(false))
                    }
                };
                if button.on_hover_text(if channel.muted { tr("Включить звук", "Unmute") } else { tr("Выключить звук", "Mute") }).clicked() {
                    shell::set_mute(target.clone(), !channel.muted);
                }
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&channel.name).font(semibold(14.0)));
                        if let Some(sub) = subtitle {
                            ui.label(RichText::new(sub).small().weak());
                        }
                    });
                    ui.horizontal(|ui| {
                        let key = channel.key.clone();
                        let mut value = match &self.dragging {
                            Some((k, v)) if *k == key => *v,
                            _ => channel.volume,
                        };
                        let width = (ui.available_width() - 60.0).max(120.0);
                        ui.spacing_mut().slider_width = width;
                        let r = ui.add(egui::Slider::new(&mut value, 0.0..=100.0).show_value(false));
                        if r.changed() {
                            self.dragging = Some((key.clone(), value));
                            shell::set_volume(target.clone(), value);
                        }
                        if r.drag_stopped() || (!r.dragged() && self.dragging.as_ref().map(|d| d.0 == key).unwrap_or(false) && !r.changed()) {
                            self.dragging = None;
                        }
                        ui.label(RichText::new(format!("{}", value.round() as i32)).font(semibold(14.0)));
                    });
                    // Индикатор уровня
                    let (rect, _) = ui.allocate_exact_size(Vec2::new((ui.available_width() - 60.0).max(120.0), 3.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 1.5, ui.visuals().widgets.inactive.bg_fill);
                    if !channel.muted {
                        let mut fill = rect;
                        fill.set_width(rect.width() * (channel.peak * channel.volume / 100.0).clamp(0.0, 1.0));
                        ui.painter().rect_filled(fill, 1.5, ui.visuals().selection.bg_fill.gamma_multiply(0.7));
                    }
                });
            });
        });
    }

    fn mixer_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading(tr("Микшер", "Mixer"));
        let snap = audio::snapshot();
        section(ui, tr("Устройство вывода", "Output device"));
        card(ui, |ui| {
            let current = snap.devices.iter().find(|d| d.id == snap.device_id).map(|d| d.name.clone()).unwrap_or_else(|| snap.device_name.clone());
            egui::ComboBox::from_id_salt("device").width(ui.available_width() - 8.0).selected_text(current).show_ui(ui, |ui| {
                for d in &snap.devices {
                    if ui.selectable_label(d.id == snap.device_id, &d.name).clicked() {
                        shell::set_device(d.id.clone());
                    }
                }
            });
            description(ui, tr("Выбранное устройство становится устройством вывода по умолчанию в Windows", "The selected device becomes the Windows default output"));
        });
        if snap.has_device {
            ui.add_space(4.0);
            self.channel_row(ui, ctx, &snap.master, Target::Master, Some(&snap.device_name));
        }
        section(ui, tr("Приложения", "Apps"));
        if snap.apps.is_empty() {
            card(ui, |ui| {
                ui.label(RichText::new(tr("Сейчас ни одно приложение не воспроизводит звук", "No app is playing sound right now")).weak());
            });
        }
        for app in &snap.apps {
            self.channel_row(ui, ctx, app, Target::App(app.key.clone()), None);
        }
    }

    // ---------- Горячие клавиши ----------

    fn stop_capture(&mut self) {
        if self.capture.take().is_some() {
            shell::suspend_hotkeys(false);
        }
    }

    fn poll_capture(&mut self, ctx: &egui::Context) {
        let Some(index) = self.capture else { return };
        ctx.request_repaint_after(Duration::from_millis(30));
        let key = hotkeys::pressed_key();
        // Ждём, пока отпустят клавишу, которой начали захват (щелчок мышью её не даёт, Enter — даёт)
        if !self.capture_armed {
            if key.is_none() {
                self.capture_armed = true;
            }
            return;
        }
        let Some(vk) = key else { return };
        let mods = hotkeys::current_modifiers();
        match vk {
            0x1B if mods == 0 => self.stop_capture(),
            0x08 | 0x2E if mods == 0 => {
                settings::update(|s| {
                    if let Some(h) = s.hotkeys.get_mut(index) {
                        h.modifiers = 0;
                        h.vk = 0;
                    }
                });
                self.stop_capture();
            }
            // Одиночная буква или цифра перехватывалась бы во всех программах и мешала печатать
            _ if mods == 0 && !hotkeys::is_standalone_key(vk) => {}
            _ => {
                settings::update(|s| {
                    if let Some(h) = s.hotkeys.get_mut(index) {
                        h.modifiers = mods;
                        h.vk = vk;
                    }
                });
                self.stop_capture();
            }
        }
    }

    fn hotkeys_page(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.poll_capture(ctx);
        ui.heading(tr("Горячие клавиши", "Hotkeys"));
        description(
            ui,
            tr("Глобальные сочетания клавиш работают везде, даже когда окно скрыто. «Активное окно» меняет громкость программы, которая сейчас на переднем плане.", "Global hotkeys work everywhere, even when the window is hidden. “Active window” changes the volume of the app in front."),
        );
        ui.add_space(6.0);
        let bindings = settings::read(|s| s.hotkeys.clone());
        let failed = hotkeys::FAILED.lock().map(|f| f.clone()).unwrap_or_default();
        let known: Vec<String> = {
            let snap = audio::snapshot();
            let mut list: Vec<String> = snap.apps.iter().filter(|a| !a.is_system).map(|a| a.key.clone()).collect();
            for b in &bindings {
                if let Some(a) = b.app.as_ref().filter(|a| !a.is_empty()) {
                    if !list.iter().any(|k| k.eq_ignore_ascii_case(a)) {
                        list.push(a.clone());
                    }
                }
            }
            list
        };
        let mut remove: Option<usize> = None;
        for (i, b) in bindings.iter().enumerate() {
            card(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let mut action = b.action;
                    egui::ComboBox::from_id_salt(("action", i)).width(210.0).selected_text(action_name(action)).show_ui(ui, |ui| {
                        for a in ACTIONS {
                            ui.selectable_value(&mut action, a, action_name(a));
                        }
                    });
                    if action != b.action {
                        settings::update(|s| {
                            if let Some(h) = s.hotkeys.get_mut(i) {
                                h.action = action;
                            }
                        });
                    }
                    if b.has_target() {
                        let mut target = b.target;
                        egui::ComboBox::from_id_salt(("target", i)).width(160.0).selected_text(target_name(target)).show_ui(ui, |ui| {
                            for t in [HotkeyTarget::Foreground, HotkeyTarget::Master, HotkeyTarget::App] {
                                ui.selectable_value(&mut target, t, target_name(t));
                            }
                        });
                        if target != b.target {
                            settings::update(|s| {
                                if let Some(h) = s.hotkeys.get_mut(i) {
                                    h.target = target;
                                }
                            });
                        }
                        if b.target == HotkeyTarget::App {
                            let mut app = b.app.clone().unwrap_or_default();
                            let r = ui.add(egui::TextEdit::singleline(&mut app).hint_text(tr("имя exe", "exe name")).desired_width(130.0));
                            egui::ComboBox::from_id_salt(("apps", i)).width(24.0).selected_text("").show_ui(ui, |ui| {
                                for k in &known {
                                    if ui.selectable_label(false, k).clicked() {
                                        app = k.clone();
                                        settings::update(|s| {
                                            if let Some(h) = s.hotkeys.get_mut(i) {
                                                h.app = Some(k.clone());
                                            }
                                        });
                                    }
                                }
                            });
                            if r.changed() {
                                let mut value = app.trim().to_string();
                                if value.to_lowercase().ends_with(".exe") {
                                    value.truncate(value.len() - 4);
                                }
                                settings::update(|s| {
                                    if let Some(h) = s.hotkeys.get_mut(i) {
                                        h.app = if value.is_empty() { None } else { Some(value.clone()) };
                                    }
                                });
                            }
                        }
                    }
                    if b.action == HotkeyAction::Launch {
                        let items = launcher_items();
                        let current = b.app.as_ref().and_then(|id| items.iter().find(|(x, _)| x == id)).map(|(_, n)| n.clone()).unwrap_or_else(|| tr("Выберите ярлык", "Choose a shortcut").into());
                        egui::ComboBox::from_id_salt(("launch", i)).width(200.0).selected_text(current).show_ui(ui, |ui| {
                            for (id, name) in &items {
                                if ui.selectable_label(b.app.as_deref() == Some(id), name).clicked() {
                                    let id = id.clone();
                                    settings::update(|s| {
                                        if let Some(h) = s.hotkeys.get_mut(i) {
                                            h.app = Some(id);
                                        }
                                    });
                                }
                            }
                        });
                    }
                    let capturing = self.capture == Some(i);
                    let label = if capturing {
                        tr("Нажмите сочетание…", "Press a shortcut…").to_string()
                    } else if b.vk == 0 {
                        tr("Не назначено", "Not set").to_string()
                    } else {
                        hotkeys::format(b.modifiers, b.vk)
                    };
                    let button = ui.add_sized([200.0, 28.0], egui::Button::new(label).selected(capturing));
                    if button.clicked() {
                        if capturing {
                            self.stop_capture();
                        } else {
                            if self.capture.is_none() {
                                shell::suspend_hotkeys(true);
                            }
                            self.capture = Some(i);
                            self.capture_armed = false;
                        }
                    }
                    button.on_hover_text(tr("Нажмите и затем сочетание клавиш. Esc — отмена, Backspace — убрать", "Click, then press a shortcut. Esc cancels, Backspace clears"));
                    if ui.add(egui::Button::new(RichText::new("\u{E74D}").font(icon_font(14.0))).frame(false)).on_hover_text(tr("Удалить", "Delete")).clicked() {
                        remove = Some(i);
                    }
                });
                if failed.iter().any(|f| f == b) {
                    ui.label(RichText::new(tr("Это сочетание уже занято другой программой — выберите другое.", "This shortcut is taken by another app — choose another.")).color(Color32::from_rgb(0xE8, 0x5D, 0x4A)).small());
                }
            });
        }
        if let Some(i) = remove {
            self.stop_capture();
            settings::update(|s| {
                if i < s.hotkeys.len() {
                    s.hotkeys.remove(i);
                }
            });
        }
        ui.add_space(6.0);
        if ui.button(tr("➕  Добавить", "➕  Add")).clicked() {
            settings::update(|s| s.hotkeys.push(HotkeyBinding::new(HotkeyAction::VolumeUp)));
        }
    }

    // ---------- Общие настройки ----------

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        let s = settings::get();
        ui.heading(tr("Настройки", "Settings"));

        section(ui, tr("Запуск", "Startup"));
        let mut autostart = self.autostart;
        let mut changed = false;
        setting_row(ui, tr("Запускать вместе с Windows", "Start with Windows"), tr("OmniFlex будет тихо стартовать в области уведомлений", "OmniFlex starts quietly in the notification area"), |ui| {
            changed = toggle(ui, &mut autostart).changed();
        });
        if changed {
            win::set_autostart(autostart);
            self.autostart = win::autostart_enabled();
        }
        toggle_setting(
            ui,
            tr("Сворачивать в трей при закрытии", "Minimize to tray on close"),
            tr("Крестик прячет окно, остров и горячие клавиши продолжают работать", "The close button hides the window; the island and hotkeys keep working"),
            s.close_to_tray,
            |s, v| s.close_to_tray = v,
        );

        section(ui, tr("Язык и оформление", "Language and appearance"));
        let mut language = s.language;
        setting_row(ui, tr("Язык", "Language"), tr("Русский на русскоязычной Windows, английский на остальных", "Russian on Russian Windows, English elsewhere"), |ui| {
            egui::ComboBox::from_id_salt("language").width(200.0).selected_text(language_name(language)).show_ui(ui, |ui| {
                for l in [Language::Auto, Language::Russian, Language::English] {
                    ui.selectable_value(&mut language, l, language_name(l));
                }
            });
        });
        if language != s.language {
            settings::update(|st| st.language = language);
        }
        let mut theme = s.theme;
        setting_row(ui, tr("Тема окон", "Window theme"), tr("Цвет акцента берётся из настроек Windows", "The accent color comes from Windows settings"), |ui| {
            theme_combo(ui, "theme", &mut theme);
        });
        if theme != s.theme {
            settings::update(|st| st.theme = theme);
            self.dark = None;
        }

        section(ui, tr("Песня и текст на рабочем столе", "Song and lyrics on the desktop"));
        toggle_setting(
            ui,
            tr("Песня и текст на рабочем столе", "Song and lyrics on the desktop"),
            tr(
                "Пока играет песня, вместо обоев — обложка и текст, строки подсвечиваются под музыку. Spotify, Яндекс Музыка и YouTube (в том числе в браузере)",
                "While a song plays, the wallpaper shows its cover and synced lyrics. Spotify, Yandex Music and YouTube (also in a browser)",
            ),
            s.desktop_lyrics,
            |s, v| s.desktop_lyrics = v,
        );
        card(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button(tr("Папка текстов", "Lyrics folder")).clicked() {
                    let dir = crate::media::lyrics::folder();
                    let _ = std::fs::create_dir_all(&dir);
                    open_path(&dir);
                }
                description(ui, tr("Файл «Исполнитель - Название.lrc» (или .txt) важнее найденного в базах", "An “Artist - Title.lrc” (or .txt) file takes priority over online databases"));
            });
        });
        let mut source = s.source;
        setting_row(ui, tr("Источник песни", "Song source"), tr("Если песня играет, а источника не видно — выберите его вручную", "If a song plays but is not detected, choose the source manually"), |ui| {
            egui::ComboBox::from_id_salt("source").width(260.0).selected_text(source_name(source)).show_ui(ui, |ui| {
                for m in [MusicSourceMode::Auto, MusicSourceMode::Spotify, MusicSourceMode::YandexMusic, MusicSourceMode::YouTube] {
                    ui.selectable_value(&mut source, m, source_name(m));
                }
            });
        });
        if source != s.source {
            settings::update(|st| st.source = source);
        }
        let monitors = win::monitors();
        let mut screen = s.desktop_screen.clone();
        setting_row(ui, tr("Мониторы", "Monitors"), tr("На каких экранах показывать песню вместо обоев", "Which screens show the song instead of the wallpaper"), |ui| {
            let label = |dev: &str| -> String {
                match dev {
                    settings::ALL_SCREENS => tr("Все мониторы", "All monitors").into(),
                    settings::PRIMARY_SCREEN => tr("Только основной", "Primary only").into(),
                    d => monitors.iter().position(|m| m.device == d).map(|i| monitor_label(i, &monitors[i])).unwrap_or_else(|| tr("Отключённый монитор", "Disconnected monitor").into()),
                }
            };
            egui::ComboBox::from_id_salt("desktop-screen").width(260.0).selected_text(label(&screen)).show_ui(ui, |ui| {
                ui.selectable_value(&mut screen, settings::ALL_SCREENS.to_string(), tr("Все мониторы", "All monitors"));
                ui.selectable_value(&mut screen, settings::PRIMARY_SCREEN.to_string(), tr("Только основной", "Primary only"));
                for (i, m) in monitors.iter().enumerate() {
                    ui.selectable_value(&mut screen, m.device.clone(), monitor_label(i, m));
                }
            });
        });
        if screen != s.desktop_screen {
            settings::update(|st| st.desktop_screen = screen.clone());
        }
        toggle_setting(
            ui,
            tr("Кнопки на рабочем столе", "Desktop buttons"),
            tr("«Назад», «Играть / пауза» и «Вперёд» под полосой прогресса", "Previous, Play/Pause and Next under the progress bar"),
            s.desktop_controls,
            |s, v| s.desktop_controls = v,
        );
        let mut speed = s.marquee_speed;
        setting_row(ui, tr("Скорость бегущей строки", "Marquee speed"), tr("По умолчанию 22", "Default 22"), |ui| {
            ui.add(egui::Slider::new(&mut speed, settings::MIN_MARQUEE_SPEED..=settings::MAX_MARQUEE_SPEED));
        });
        if speed != s.marquee_speed {
            settings::update(|st| st.marquee_speed = speed);
        }
        let mut fps = s.marquee_fps;
        setting_row(ui, tr("Кадров в секунду у бегущей строки", "Marquee frames per second"), tr("По умолчанию 60", "Default 60"), |ui| {
            ui.add(egui::Slider::new(&mut fps, settings::MIN_MARQUEE_FPS..=settings::MAX_MARQUEE_FPS));
        });
        if fps != s.marquee_fps {
            settings::update(|st| st.marquee_fps = fps);
        }

        section(ui, tr("Громкость", "Volume"));
        let mut step = s.volume_step;
        setting_row(ui, tr("Шаг изменения громкости", "Volume step"), tr("Для горячих клавиш и колёсика мыши", "For hotkeys and the mouse wheel"), |ui| {
            ui.add(egui::Slider::new(&mut step, 1..=25));
        });
        if step != s.volume_step {
            settings::update(|st| st.volume_step = step);
        }
        toggle_setting(ui, tr("Всплывающий индикатор", "Volume indicator"), tr("Показывать громкость внизу экрана при нажатии горячих клавиш", "Show volume at the bottom of the screen on hotkeys"), s.show_osd, |s, v| s.show_osd = v);

        section(ui, tr("О программе", "About"));
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new(util::APP_NAME).font(semibold(15.0)));
                    description(ui, &format!("{} {}", tr("Версия", "Version"), util::VERSION));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(tr("Папка настроек", "Settings folder")).clicked() {
                        let dir = util::data_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        open_path(&dir);
                    }
                });
            });
        });
    }

    // ---------- Остров ----------

    fn island_page(&mut self, ui: &mut egui::Ui) {
        let s = settings::get();
        let i = &s.island;
        ui.heading(tr("Остров", "Island"));

        section(ui, tr("Остров", "Island"));
        toggle_setting(
            ui,
            tr("Остров вверху экрана", "Island at the top of the screen"),
            tr("Пилюля с часами и значками; под курсором раскрывается в панель с плеером, громкостью, инструментами и лаунчером", "A pill with the clock and indicators; under the cursor it opens a panel with the player, volume, tools and launcher"),
            i.enabled,
            |s, v| s.island.enabled = v,
        );
        let mut theme = i.theme;
        setting_row(ui, tr("Тема острова", "Island theme"), "", |ui| theme_combo(ui, "island-theme", &mut theme));
        if theme != i.theme {
            settings::update(|st| st.island.theme = theme);
        }
        int_slider(ui, tr("Размер, %", "Size, %"), tr("Или Ctrl + колёсико мыши над островом", "Or Ctrl + mouse wheel over the island"), i.size, 80..=200, |s, v| s.island.size = v);
        int_slider(ui, tr("Непрозрачность свёрнутого, %", "Collapsed opacity, %"), "", i.opacity_collapsed, 20..=100, |s, v| s.island.opacity_collapsed = v);
        int_slider(ui, tr("Непрозрачность раскрытого, %", "Expanded opacity, %"), "", i.opacity_expanded, 20..=100, |s, v| s.island.opacity_expanded = v);

        let monitors = win::monitors();
        let mut island_screen = i.screen.clone();
        setting_row(ui, tr("Монитор", "Monitor"), tr("С панелью задач вверху экрана остров встаёт под неё", "With a top taskbar the island sits below it"), |ui| {
            let label = |dev: &str| -> String {
                if dev.is_empty() {
                    return tr("Основной", "Primary").into();
                }
                monitors.iter().position(|m| m.device == dev).map(|i| monitor_label(i, &monitors[i])).unwrap_or_else(|| tr("Отключённый монитор", "Disconnected monitor").into())
            };
            egui::ComboBox::from_id_salt("island-screen").width(240.0).selected_text(label(&island_screen)).show_ui(ui, |ui| {
                ui.selectable_value(&mut island_screen, String::new(), tr("Основной", "Primary"));
                for (n, m) in monitors.iter().enumerate() {
                    ui.selectable_value(&mut island_screen, m.device.clone(), monitor_label(n, m));
                }
            });
        });
        if island_screen != i.screen {
            settings::update(|st| st.island.screen = island_screen.clone());
        }
        let mut offset = i.offset;
        let mut offset_done = false;
        setting_row(ui, tr("Сдвиг вдоль края, %", "Offset along the edge, %"), tr("0 — по центру. Остров можно и просто перетащить мышью", "0 is the center. You can also just drag the island"), |ui| {
            let r = ui.add(egui::Slider::new(&mut offset, -45.0..=45.0).fixed_decimals(1));
            offset_done = r.drag_stopped() || (r.changed() && !r.dragged());
        });
        if offset_done && offset != i.offset {
            settings::update(|s| s.island.offset = offset);
        }
        int_slider(ui, tr("Отступ сверху, точки", "Distance from the top, points"), tr("0 — у самого края", "0 is right at the edge"), i.offset_y, 0..=600, |s, v| s.island.offset_y = v);
        toggle_setting(
            ui,
            tr("Сквозной режим", "Click-through mode"),
            tr(
                "Свёрнутый остров пропускает клики и не раскрывается под курсором — раскрыть его можно, зажав Ctrl над ним. Переключается и клавишей, и командой OmniFlex.exe --click-through on|off|toggle",
                "The collapsed island lets clicks through and doesn't open on hover — hold Ctrl over it to open it. Also a hotkey and OmniFlex.exe --click-through on|off|toggle",
            ),
            i.click_through,
            |s, v| s.island.click_through = v,
        );
        toggle_setting(
            ui,
            tr("Автоскрытие", "Auto-hide"),
            tr(
                "В покое остров уезжает за верхний край экрана и появляется, когда к нему подводят курсор. Карточки, напоминания, запись и громкость показываются как обычно",
                "When idle the island slides off the top edge and comes back when the cursor approaches. Cards, reminders, recording and volume still show up",
            ),
            i.auto_hide,
            |s, v| s.island.auto_hide = v,
        );
        if i.auto_hide {
            int_slider(ui, tr("Прятать через, с", "Hide after, s"), tr("Сколько ждать после ухода курсора", "How long to wait after the cursor leaves"), i.auto_hide_delay, 1..=10, |s, v| s.island.auto_hide_delay = v);
        }
        toggle_setting(
            ui,
            tr("Остров на скриншотах и в записи", "Island in screenshots and recordings"),
            tr("По умолчанию остров в захват экрана не попадает — ни в скриншоты, ни в запись, ни в трансляции", "By default the island is excluded from screen capture, recordings and streams"),
            i.show_in_capture,
            |s, v| s.island.show_in_capture = v,
        );
        toggle_setting(ui, tr("Прятать во время игр", "Hide during games"), tr("В играх и полноэкранных окнах острова нет", "No island in games and full-screen windows"), i.hide_in_games, |s, v| s.island.hide_in_games = v);
        toggle_setting(ui, tr("Громкость колёсиком", "Wheel changes volume"), tr("Колёсико мыши над островом меняет громкость", "The mouse wheel over the island changes volume"), i.wheel_volume, |s, v| s.island.wheel_volume = v);

        section(ui, tr("Что показывать", "What to show"));
        toggle_setting(ui, tr("Заряд батареи", "Battery"), "", i.show_battery, |s, v| s.island.show_battery = v);
        toggle_setting(ui, tr("Раскладка клавиатуры", "Keyboard layout"), tr("При переключении остров на миг раскрывается", "The island briefly opens when you switch"), i.show_layout, |s, v| s.island.show_layout = v);
        toggle_setting(ui, tr("Микрофон и камера", "Microphone and camera"), tr("Оранжевая точка — программа слушает микрофон, зелёная — смотрит в камеру", "Orange dot — an app uses the microphone, green — the camera"), i.show_mic_cam, |s, v| s.island.show_mic_cam = v);
        toggle_setting(ui, tr("Секунды на часах", "Seconds on the clock"), "", i.show_seconds, |s, v| s.island.show_seconds = v);

        section(ui, tr("Музыка", "Music"));
        let mut music = i.music;
        setting_row(ui, tr("Пилюля во время музыки", "Pill while music plays"), "", |ui| {
            egui::ComboBox::from_id_salt("music").width(240.0).selected_text(music_name(music)).show_ui(ui, |ui| {
                for m in [MusicPill::Off, MusicPill::Wide, MusicPill::Mini] {
                    ui.selectable_value(&mut music, m, music_name(m));
                }
            });
        });
        if music != i.music {
            settings::update(|st| st.island.music = music);
        }
        toggle_setting(
            ui,
            tr("Текст песни в такт музыке", "Synced lyrics"),
            tr("В мини-плеере и в своей пилюле подсвечивается строка, которая звучит сейчас. Тексты — из открытой базы LRCLIB и других, только для играющей песни", "The current line shows in the mini player and its own pill. Lyrics come from LRCLIB and other open databases, only for the playing song"),
            i.lyrics,
            |s, v| s.island.lyrics = v,
        );
        toggle_setting(ui, tr("Край-эквалайзер", "Equalizer edge"), tr("При музыке нижний край показывает звук по частотам", "While music plays the bottom edge shows the spectrum"), i.equalizer, |s, v| s.island.equalizer = v);
        if i.equalizer {
            let mut style = i.eq_style;
            setting_row(ui, tr("Вид эквалайзера", "Equalizer style"), tr("Цвет берётся из обложки трека", "The color comes from the track cover"), |ui| {
                egui::ComboBox::from_id_salt("eq-style").width(240.0).selected_text(eq_name(style)).show_ui(ui, |ui| {
                    for e in [EqStyle::Bars, EqStyle::Wave, EqStyle::Dots, EqStyle::Glow, EqStyle::Rainbow, EqStyle::Mirror] {
                        ui.selectable_value(&mut style, e, eq_name(e));
                    }
                });
            });
            if style != i.eq_style {
                settings::update(|st| st.island.eq_style = style);
            }
        }
        let players = crate::media::player::state().players;
        let mut player = i.player.clone();
        setting_row(ui, tr("Плеер острова", "Island player"), tr("Когда открыто несколько плееров: какой показывать и каким управлять. То же — кнопкой в плеере острова", "With several players open: which one the island shows and controls"), |ui| {
            let current = if player.is_empty() {
                tr("Тот, что играет", "Whichever is playing").to_string()
            } else {
                crate::media::player::app_name(&player)
            };
            egui::ComboBox::from_id_salt("player").width(240.0).selected_text(current).show_ui(ui, |ui| {
                ui.selectable_value(&mut player, String::new(), tr("Тот, что играет", "Whichever is playing"));
                for p in &players {
                    ui.selectable_value(&mut player, p.aumid.clone(), &p.app);
                }
            });
        });
        if player != i.player {
            settings::update(|st| st.island.player = player.clone());
        }

        section(ui, tr("Свои устройства звука", "Custom audio devices"));
        description(ui, tr("Чью громкость регулирует остров и какой микрофон выключает и записывает, если это не устройства Windows по умолчанию (удобно с виртуальным микшером). Сам звук никуда не переключается", "Which device's volume the island controls and which microphone it mutes and records, if not the Windows defaults (handy with a virtual mixer). Audio itself is not rerouted"));
        let snap = audio::snapshot();
        let mut out = i.output_device.clone();
        setting_row(ui, tr("Устройство вывода", "Output device"), "", |ui| device_combo(ui, "out-dev", &mut out, &snap.devices));
        if out != i.output_device {
            settings::update(|st| st.island.output_device = out.clone());
        }
        let mut mic = i.mic_device.clone();
        setting_row(ui, tr("Микрофон", "Microphone"), "", |ui| device_combo(ui, "mic-dev", &mut mic, &snap.mics));
        if mic != i.mic_device {
            settings::update(|st| st.island.mic_device = mic.clone());
        }

        section(ui, tr("Уведомления Windows", "Windows notifications"));
        toggle_setting(
            ui,
            tr("Уведомления на острове", "Notifications on the island"),
            tr("Сообщения и уведомления других программ показываются карточкой; нажатие открывает программу", "Messages and notifications from other apps appear as a card; click to open the app"),
            i.notifications,
            |s, v| s.island.notifications = v,
        );
        let mut excluded = i.notifications_excluded.join(", ");
        let mut changed = false;
        setting_row(ui, tr("Не показывать от программ", "Don't show from apps"), tr("Через запятую, как программа называется в уведомлении", "Comma-separated, as the app is named in the notification"), |ui| {
            changed = ui.add(egui::TextEdit::singleline(&mut excluded).desired_width(260.0)).changed();
        });
        if changed {
            let list: Vec<String> = excluded.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
            settings::update(|st| st.island.notifications_excluded = list);
        }
    }

    // ---------- Инструменты ----------

    fn tools_page(&mut self, ui: &mut egui::Ui) {
        let s = settings::get();
        let t = &s.tools;
        ui.heading(tr("Инструменты", "Tools"));
        description(ui, tr("Все инструменты — на раскрытом острове и на горячих клавишах (страница «Горячие клавиши»)", "All tools are on the expanded island and on hotkeys (Hotkeys page)"));

        section(ui, tr("Скриншоты", "Screenshots"));
        toggle_setting(ui, tr("Копировать в буфер", "Copy to clipboard"), "", t.copy_screenshots, |s, v| s.tools.copy_screenshots = v);
        toggle_setting(ui, tr("Сохранять в папку", "Save to folder"), "", t.save_screenshots, |s, v| s.tools.save_screenshots = v);
        folder_row(ui, tr("Папка скриншотов", "Screenshot folder"), &t.screenshot_dir, crate::tools::screenshot_folder(), |s, v| s.tools.screenshot_dir = v);

        section(ui, tr("Запись экрана", "Screen recording"));
        let mut format = t.record_format;
        setting_row(ui, tr("Формат", "Format"), tr("MP4 — H.264 через Media Foundation; GIF — до 10 кадров в секунду", "MP4 — H.264 via Media Foundation; GIF — up to 10 fps"), |ui| {
            ui.selectable_value(&mut format, RecordFormat::Gif, "GIF");
            ui.selectable_value(&mut format, RecordFormat::Mp4, "MP4");
        });
        if format != t.record_format {
            settings::update(|st| st.tools.record_format = format);
        }
        let mut fps = t.record_fps;
        setting_row(ui, tr("Кадров в секунду (MP4)", "Frames per second (MP4)"), "", |ui| {
            ui.selectable_value(&mut fps, 60, "60");
            ui.selectable_value(&mut fps, 30, "30");
        });
        if fps != t.record_fps {
            settings::update(|st| st.tools.record_fps = fps);
        }
        toggle_setting(ui, tr("Высокое качество", "High quality"), tr("Выше битрейт — больше файл", "Higher bitrate, bigger file"), t.record_high_quality, |s, v| s.tools.record_high_quality = v);
        folder_row(ui, tr("Папка записей", "Recordings folder"), &t.record_dir, crate::tools::record::folder(), |s, v| s.tools.record_dir = v);

        section(ui, tr("Ссылка на скриншот", "Screenshot link"));
        description(ui, tr("Кнопка «Ссылка» на карточке скриншота загружает его и копирует ссылку. Пока сервис не выбран, ничего никуда не загружается", "The Link button on a screenshot card uploads it and copies the link. Nothing is uploaded until a service is chosen"));
        let mut service = t.upload;
        setting_row(ui, tr("Сервис", "Service"), "", |ui| {
            egui::ComboBox::from_id_salt("upload").width(200.0).selected_text(upload_name(service)).show_ui(ui, |ui| {
                for u in [UploadService::None, UploadService::Imgur, UploadService::ImgBB] {
                    ui.selectable_value(&mut service, u, upload_name(u));
                }
            });
        });
        if service != t.upload {
            settings::update(|st| st.tools.upload = service);
        }
        match t.upload {
            UploadService::Imgur => text_setting(ui, "Imgur Client ID", "api.imgur.com/oauth2/addclient", &t.imgur_client_id, false, |s, v| s.tools.imgur_client_id = v),
            UploadService::ImgBB => text_setting(ui, tr("Ключ ImgBB", "ImgBB key"), "api.imgbb.com", &t.imgbb_key, true, |s, v| s.tools.imgbb_key = v),
            UploadService::None => {}
        }
    }

    // ---------- Напоминания ----------

    fn reminders_page(&mut self, ui: &mut egui::Ui) {
        ui.heading(tr("Напоминания", "Reminders"));
        description(ui, tr("Появляются карточкой со звуком на острове и ждут, пока на них не ответят", "They appear as a card with a sound on the island and wait for an answer"));
        section(ui, tr("Новое", "New"));
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.new_reminder.0).hint_text(tr("О чём напомнить", "What to remind about")).desired_width(260.0));
                ui.add(egui::TextEdit::singleline(&mut self.new_reminder.1).hint_text(tr("ЧЧ:ММ", "HH:MM")).desired_width(60.0));
                ui.add(egui::TextEdit::singleline(&mut self.new_reminder.2).hint_text(tr("дни: будни, пн ср… (пусто — один раз)", "days: weekdays, mon wed… (empty — once)")).desired_width(240.0));
            });
            let time = crate::reminders::parse_time(&self.new_reminder.1);
            let days = crate::reminders::parse_days(&self.new_reminder.2);
            ui.horizontal(|ui| {
                let when = match time {
                    Some(t) if days.is_empty() => {
                        let now = chrono::Local::now().naive_local();
                        let today = now.date().and_time(t);
                        let at = if today > now { today } else { today + chrono::Duration::days(1) };
                        format!("{} {}", tr("Сработает", "Fires"), at.format("%d.%m %H:%M"))
                    }
                    Some(t) => format!("{} {} {}", tr("Сработает", "Fires"), crate::reminders::days_label(&days), t.format("%H:%M")),
                    None => tr("Укажите время", "Enter a time").into(),
                };
                description(ui, &when);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let ok = time.is_some() && !self.new_reminder.0.trim().is_empty();
                    if ui.add_enabled(ok, egui::Button::new(tr("Добавить", "Add"))).clicked() {
                        let t = time.unwrap();
                        let now = chrono::Local::now().naive_local();
                        let today = now.date().and_time(t);
                        let at = if today > now { today } else { today + chrono::Duration::days(1) };
                        let r = settings::Reminder {
                            id: settings::new_id(),
                            text: self.new_reminder.0.trim().to_string(),
                            time: t.format("%H:%M").to_string(),
                            date: if days.is_empty() { at.format("%Y-%m-%d").to_string() } else { String::new() },
                            days: days.clone(),
                            enabled: true,
                            fired: String::new(),
                        };
                        settings::update(|s| s.reminders.push(r));
                        self.new_reminder = Default::default();
                    }
                });
            });
        });
        section(ui, tr("Все напоминания", "All reminders"));
        let list = settings::read(|s| s.reminders.clone());
        if list.is_empty() {
            card(ui, |ui| {
                ui.label(RichText::new(tr("Напоминаний пока нет", "No reminders yet")).weak());
            });
        }
        let mut remove = None;
        for r in &list {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    let mut on = r.enabled;
                    if toggle(ui, &mut on).changed() {
                        let id = r.id.clone();
                        settings::update(|s| {
                            if let Some(x) = s.reminders.iter_mut().find(|x| x.id == id) {
                                x.enabled = on;
                            }
                        });
                    }
                    ui.label(RichText::new(&r.text).font(semibold(14.0)));
                    let when = if r.days.is_empty() { format!("{} {}", r.date, r.time) } else { format!("{} {}", crate::reminders::days_label(&r.days), r.time) };
                    ui.label(RichText::new(when).weak());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.add(egui::Button::new(RichText::new("\u{E74D}").font(icon_font(14.0))).frame(false)).on_hover_text(tr("Удалить", "Delete")).clicked() {
                            remove = Some(r.id.clone());
                        }
                    });
                });
            });
        }
        if let Some(id) = remove {
            settings::update(|s| s.reminders.retain(|x| x.id != id));
        }
    }

    // ---------- ИИ, голос и агенты ----------

    fn ai_page(&mut self, ui: &mut egui::Ui) {
        let s = settings::get();
        ui.heading(tr("ИИ и голос", "AI and voice"));

        section(ui, tr("ИИ", "AI"));
        description(ui, tr("Любой OpenAI-совместимый API. Бесплатные сервисы — одним нажатием; Pollinations работает без ключа", "Any OpenAI-compatible API. Free services in one click; Pollinations needs no key"));
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for p in crate::ai::PRESETS.iter() {
                    let active = s.ai.base_url == p.base_url;
                    if ui.add(egui::Button::new(p.name).selected(active)).clicked() {
                        settings::update(|st| {
                            st.ai.base_url = p.base_url.into();
                            st.ai.model = p.model.into();
                            st.ai.transcribe_model = p.transcribe_model.into();
                        });
                        if !p.key_url.is_empty() && s.ai.api_key.is_empty() {
                            open_url(p.key_url);
                        }
                    }
                }
            });
        });
        text_setting(ui, tr("Адрес API", "API URL"), "", &s.ai.base_url, false, |st, v| st.ai.base_url = v);
        text_setting(ui, tr("Ключ", "Key"), tr("Не нужен для Pollinations", "Not needed for Pollinations"), &s.ai.api_key, true, |st, v| st.ai.api_key = v);
        text_setting(ui, tr("Модель", "Model"), "", &s.ai.model, false, |st, v| st.ai.model = v);
        text_setting(ui, tr("Модель распознавания речи", "Speech model"), tr("Whisper, например whisper-1 у OpenAI", "Whisper, e.g. whisper-1 on OpenAI"), &s.ai.transcribe_model, false, |st, v| st.ai.transcribe_model = v);

        section(ui, tr("Голос в текст", "Voice to text"));
        let mut engine = s.voice.engine;
        setting_row(ui, tr("Распознавание", "Recognition"), tr("Windows — бесплатно, нужно включить «Распознавание голоса в сети» в Параметрах. Whisper — через API выше", "Windows — free, needs Online speech recognition turned on. Whisper — via the API above"), |ui| {
            ui.selectable_value(&mut engine, VoiceEngine::Api, "Whisper");
            ui.selectable_value(&mut engine, VoiceEngine::Windows, "Windows");
        });
        if engine != s.voice.engine {
            settings::update(|st| st.voice.engine = engine);
        }
        toggle_setting(ui, tr("Сразу вставлять текст", "Paste right away"), tr("Текст попадает не только в буфер, но и в окно, где стоял курсор", "Text goes to the clipboard and into the window where the cursor was"), s.voice.paste, |st, v| st.voice.paste = v);

        section(ui, tr("ИИ-агенты в терминале", "AI agents in the terminal"));
        description(ui, tr("Claude Code и Codex сообщают острову, что делают: работает, ждёт ответа, закончил. Claude Code можно ответить прямо на острове", "Claude Code and Codex tell the island what they're doing. You can answer Claude Code right on the island"));
        let claude = crate::agents::claude_connected();
        let mut on = claude;
        let mut changed = false;
        setting_row(ui, "Claude Code", tr("Добавляет хуки в %USERPROFILE%\\.claude\\settings.json, остальные не трогает", "Adds hooks to %USERPROFILE%\\.claude\\settings.json, leaving others intact"), |ui| {
            changed = toggle(ui, &mut on).changed();
        });
        if changed {
            self.agent_error = crate::agents::connect_claude(on).err();
        }
        let codex = crate::agents::codex_connected();
        let mut on = codex;
        let mut changed = false;
        setting_row(ui, "Codex", tr("Строка notify в %USERPROFILE%\\.codex\\config.toml", "The notify line in %USERPROFILE%\\.codex\\config.toml"), |ui| {
            changed = toggle(ui, &mut on).changed();
        });
        if changed {
            self.agent_error = crate::agents::connect_codex(on).err();
        }
        if let Some(e) = &self.agent_error {
            ui.label(RichText::new(e).color(Color32::from_rgb(0xE8, 0x5D, 0x4A)).small());
        }
    }
}

fn theme_combo(ui: &mut egui::Ui, id: &str, theme: &mut ThemeMode) {
    egui::ComboBox::from_id_salt(id).width(180.0).selected_text(theme_name(*theme)).show_ui(ui, |ui| {
        for t in [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark] {
            ui.selectable_value(theme, t, theme_name(t));
        }
    });
}

fn device_combo(ui: &mut egui::Ui, id: &str, value: &mut String, devices: &[audio::Device]) {
    let current = if value.is_empty() {
        tr("По умолчанию в Windows", "Windows default").to_string()
    } else {
        devices.iter().find(|d| d.id == *value).map(|d| d.name.clone()).unwrap_or_else(|| tr("Отключено", "Disconnected").into())
    };
    egui::ComboBox::from_id_salt(id).width(280.0).selected_text(current).show_ui(ui, |ui| {
        ui.selectable_value(value, String::new(), tr("По умолчанию в Windows", "Windows default"));
        for d in devices {
            ui.selectable_value(value, d.id.clone(), &d.name);
        }
    });
}

fn int_slider(ui: &mut egui::Ui, title: &str, desc: &str, value: i32, range: std::ops::RangeInclusive<i32>, set: impl FnOnce(&mut settings::Settings, i32)) {
    let mut v = value;
    let mut done = false;
    setting_row(ui, title, desc, |ui| {
        let r = ui.add(egui::Slider::new(&mut v, range));
        done = r.drag_stopped() || (r.changed() && !r.dragged());
    });
    if done && v != value {
        settings::update(|s| set(s, v));
    }
}

fn text_setting(ui: &mut egui::Ui, title: &str, desc: &str, value: &str, password: bool, set: impl FnOnce(&mut settings::Settings, String)) {
    let mut v = value.to_string();
    let mut changed = false;
    setting_row(ui, title, desc, |ui| {
        changed = ui.add(egui::TextEdit::singleline(&mut v).password(password).desired_width(280.0)).changed();
    });
    if changed {
        settings::update(|s| set(s, v.trim().to_string()));
    }
}

fn folder_row(ui: &mut egui::Ui, title: &str, value: &str, actual: std::path::PathBuf, set: impl FnOnce(&mut settings::Settings, String)) {
    let mut v = value.to_string();
    let mut changed = false;
    setting_row(ui, title, &actual.to_string_lossy(), |ui| {
        if ui.button(tr("Открыть", "Open")).clicked() {
            let _ = std::fs::create_dir_all(&actual);
            open_path(&actual);
        }
        changed = ui.add(egui::TextEdit::singleline(&mut v).hint_text(tr("по умолчанию", "default")).desired_width(200.0)).changed();
    });
    if changed {
        settings::update(|s| set(s, v.trim().to_string()));
    }
}

const ACTIONS: [HotkeyAction; 18] = [
    HotkeyAction::VolumeUp,
    HotkeyAction::VolumeDown,
    HotkeyAction::ToggleMute,
    HotkeyAction::OpenMixer,
    HotkeyAction::NextDevice,
    HotkeyAction::ToggleMic,
    HotkeyAction::ToggleIsland,
    HotkeyAction::ClickThrough,
    HotkeyAction::ScreenshotRegion,
    HotkeyAction::ScreenshotFull,
    HotkeyAction::ScreenText,
    HotkeyAction::QrCode,
    HotkeyAction::ColorPicker,
    HotkeyAction::Record,
    HotkeyAction::PastePlain,
    HotkeyAction::Voice,
    HotkeyAction::AiCommand,
    HotkeyAction::Launch,
];

fn action_name(a: HotkeyAction) -> &'static str {
    match a {
        HotkeyAction::VolumeUp => tr("Громче", "Volume up"),
        HotkeyAction::VolumeDown => tr("Тише", "Volume down"),
        HotkeyAction::ToggleMute => tr("Вкл/выкл звук", "Toggle mute"),
        HotkeyAction::OpenMixer => tr("Открыть микшер", "Open mixer"),
        HotkeyAction::NextDevice => tr("Следующее устройство вывода", "Next output device"),
        HotkeyAction::ToggleMic => tr("Вкл/выкл микрофон", "Toggle microphone"),
        HotkeyAction::ToggleIsland => tr("Показать/спрятать остров", "Show/hide island"),
        HotkeyAction::ClickThrough => tr("Сквозной режим острова", "Island click-through"),
        HotkeyAction::ScreenshotRegion => tr("Скриншот области", "Region screenshot"),
        HotkeyAction::ScreenshotFull => tr("Скриншот всего экрана", "Full-screen screenshot"),
        HotkeyAction::ScreenText => tr("Текст с экрана", "Screen text"),
        HotkeyAction::QrCode => tr("QR-код с экрана", "QR code from screen"),
        HotkeyAction::ColorPicker => tr("Пипетка", "Color picker"),
        HotkeyAction::Record => tr("Запись экрана", "Screen recording"),
        HotkeyAction::PastePlain => tr("Вставить без форматирования", "Paste as plain text"),
        HotkeyAction::Voice => tr("Голос в текст", "Voice to text"),
        HotkeyAction::AiCommand => tr("Спросить ИИ", "Ask AI"),
        HotkeyAction::Launch => tr("Открыть ярлык лаунчера", "Open launcher shortcut"),
    }
}

fn target_name(t: HotkeyTarget) -> &'static str {
    match t {
        HotkeyTarget::Foreground => tr("Активное окно", "Active window"),
        HotkeyTarget::Master => tr("Общая громкость", "Master volume"),
        HotkeyTarget::App => tr("Приложение…", "Application…"),
    }
}

fn source_name(m: MusicSourceMode) -> &'static str {
    match m {
        MusicSourceMode::Auto => tr("Spotify, Яндекс Музыка и YouTube", "Spotify, Yandex Music and YouTube"),
        MusicSourceMode::Spotify => tr("Только Spotify", "Spotify only"),
        MusicSourceMode::YandexMusic => tr("Яндекс Музыка (и браузер)", "Yandex Music (and browser)"),
        MusicSourceMode::YouTube => tr("YouTube (и браузер)", "YouTube (and browser)"),
    }
}

fn theme_name(t: ThemeMode) -> &'static str {
    match t {
        ThemeMode::System => tr("Как в системе", "System"),
        ThemeMode::Light => tr("Светлая", "Light"),
        ThemeMode::Dark => tr("Тёмная", "Dark"),
    }
}

fn language_name(l: Language) -> &'static str {
    match l {
        Language::Auto => tr("Как в Windows", "Same as Windows"),
        Language::Russian => "Русский",
        Language::English => "English",
    }
}

fn eq_name(e: EqStyle) -> &'static str {
    match e {
        EqStyle::Bars => tr("Столбики", "Bars"),
        EqStyle::Wave => tr("Волна", "Wave"),
        EqStyle::Dots => tr("Точки", "Dots"),
        EqStyle::Glow => tr("Светящаяся кромка", "Glowing edge"),
        EqStyle::Rainbow => tr("Радуга", "Rainbow"),
        EqStyle::Mirror => tr("Зеркало от центра", "Mirror from the center"),
    }
}

fn music_name(m: MusicPill) -> &'static str {
    match m {
        MusicPill::Off => tr("Обычная", "Normal"),
        MusicPill::Wide => tr("Пошире: трек и полоса времени", "Wide: track and time bar"),
        MusicPill::Mini => tr("Мини-плеер", "Mini player"),
    }
}

fn upload_name(u: UploadService) -> &'static str {
    match u {
        UploadService::None => tr("Не выбран", "None"),
        UploadService::Imgur => "Imgur",
        UploadService::ImgBB => "ImgBB",
    }
}

fn monitor_label(i: usize, m: &win::Monitor) -> String {
    format!(
        "{} {} — {}×{}{}",
        tr("Монитор", "Monitor"),
        i + 1,
        m.bounds.right - m.bounds.left,
        m.bounds.bottom - m.bounds.top,
        if m.primary { tr(", основной", ", primary") } else { "" }
    )
}

fn open_path(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

fn open_url(url: &str) {
    let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler", url]).spawn();
}

/// Все ярлыки лаунчера (и внутри групп) для горячих клавиш: id и имя.
fn launcher_items() -> Vec<(String, String)> {
    fn walk(items: &[settings::LauncherItem], out: &mut Vec<(String, String)>) {
        for it in items {
            if it.is_group() {
                walk(&it.children, out);
            } else {
                out.push((it.id.clone(), it.name.clone()));
            }
        }
    }
    let mut out = Vec::new();
    settings::read(|s| walk(&s.launcher, &mut out));
    out
}
