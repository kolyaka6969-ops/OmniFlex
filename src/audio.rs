//! Микшер громкости на Core Audio: устройство вывода, общая громкость, громкость каждого приложения,
//! индикаторы уровня, микрофон. Объекты COM живут в потоке окон; остальным потокам (окно настроек)
//! достаётся снимок состояния и команды через очередь потока окон.

// Методы IPolicyConfig названы как в COM
#![allow(non_snake_case)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[allow(unused_imports)]
use windows::core::{interface, Interface, IUnknown, IUnknown_Vtbl, GUID, HRESULT, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::Endpoints::{IAudioEndpointVolume, IAudioMeterInformation};
use windows::Win32::Media::Audio::{
    eCapture, eCommunications, eConsole, eMultimedia, eRender, AudioSessionStateExpired, IAudioSessionControl,
    IAudioSessionControl2, IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator, ISimpleAudioVolume,
    MMDeviceEnumerator, DEVICE_STATE_ACTIVE, ERole,
};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};

use crate::process_info::{self, AppIcon};
use crate::util::wide;

pub const SYSTEM_KEY: &str = "#system";

#[derive(Clone, Debug, Default)]
pub struct Channel {
    pub key: String,
    pub name: String,
    pub icon: Option<Arc<AppIcon>>,
    /// 0..100
    pub volume: f32,
    pub muted: bool,
    /// 0..1, с плавным затуханием как у системного индикатора.
    pub peak: f32,
    pub is_system: bool,
}

impl Channel {
    /// Значок громкости Segoe Fluent по уровню.
    pub fn glyph(&self) -> char {
        volume_glyph(self.volume, self.muted)
    }
}

pub fn volume_glyph(volume: f32, muted: bool) -> char {
    if muted {
        '\u{E74F}'
    } else if volume <= 0.0 {
        '\u{E992}'
    } else if volume < 34.0 {
        '\u{E993}'
    } else if volume < 67.0 {
        '\u{E994}'
    } else {
        '\u{E995}'
    }
}

#[derive(Clone, Debug)]
pub struct Device {
    pub id: String,
    pub name: String,
}

impl Device {
    /// Наушники или динамики — по названию.
    pub fn glyph(&self) -> char {
        let n = self.name.to_lowercase();
        if n.contains("науш") || n.contains("head") {
            '\u{E7F6}'
        } else {
            '\u{E7F5}'
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub has_device: bool,
    pub device_id: String,
    pub device_name: String,
    pub master: Channel,
    pub apps: Vec<Channel>,
    pub devices: Vec<Device>,
    /// Микрофоны (для выбора в настройках острова).
    pub mics: Vec<Device>,
    pub mic_available: bool,
    pub mic_muted: bool,
    /// Растёт при каждом обновлении — окна понимают, что пора перерисоваться.
    pub version: u64,
}

pub static SNAPSHOT: Mutex<Option<Snapshot>> = Mutex::new(None);

pub fn snapshot() -> Snapshot {
    SNAPSHOT.lock().ok().and_then(|s| s.clone()).unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Master,
    App(String),
}

/// Недокументированный интерфейс, которым и сама Windows назначает устройство по умолчанию.
#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    unsafe fn GetMixFormat(&self, a: PCWSTR, b: *mut *mut core::ffi::c_void) -> HRESULT;
    unsafe fn GetDeviceFormat(&self, a: PCWSTR, b: i32, c: *mut *mut core::ffi::c_void) -> HRESULT;
    unsafe fn ResetDeviceFormat(&self, a: PCWSTR) -> HRESULT;
    unsafe fn SetDeviceFormat(&self, a: PCWSTR, b: *mut core::ffi::c_void, c: *mut core::ffi::c_void) -> HRESULT;
    unsafe fn GetProcessingPeriod(&self, a: PCWSTR, b: i32, c: *mut i64, d: *mut i64) -> HRESULT;
    unsafe fn SetProcessingPeriod(&self, a: PCWSTR, b: *mut i64) -> HRESULT;
    unsafe fn GetShareMode(&self, a: PCWSTR, b: *mut core::ffi::c_void) -> HRESULT;
    unsafe fn SetShareMode(&self, a: PCWSTR, b: *mut core::ffi::c_void) -> HRESULT;
    unsafe fn GetPropertyValue(&self, a: PCWSTR, b: *const core::ffi::c_void, c: *mut core::ffi::c_void) -> HRESULT;
    unsafe fn SetPropertyValue(&self, a: PCWSTR, b: *const core::ffi::c_void, c: *mut core::ffi::c_void) -> HRESULT;
    unsafe fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
    unsafe fn SetEndpointVisibility(&self, a: PCWSTR, b: i32) -> HRESULT;
}

const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// Делает устройство устройством вывода по умолчанию во всех ролях.
pub fn set_default_device(id: &str) -> bool {
    unsafe {
        let Ok(policy) = CoCreateInstance::<_, IPolicyConfig>(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL) else {
            return false;
        };
        let id_w = wide(id);
        let mut ok = true;
        for role in [eConsole, eMultimedia, eCommunications] {
            ok &= policy.SetDefaultEndpoint(PCWSTR(id_w.as_ptr()), role).is_ok();
        }
        ok
    }
}

struct Session {
    /// Держит сеанс, пока он в списке.
    #[allow(dead_code)]
    control: IAudioSessionControl,
    volume: ISimpleAudioVolume,
    meter: Option<IAudioMeterInformation>,
}

struct AppChannel {
    channel: Channel,
    sessions: Vec<Session>,
}

pub struct Mixer {
    enumerator: Option<IMMDeviceEnumerator>,
    device: Option<IMMDevice>,
    device_id: String,
    device_name: String,
    endpoint: Option<IAudioEndpointVolume>,
    meter: Option<IAudioMeterInformation>,
    manager: Option<IAudioSessionManager2>,
    master: Channel,
    apps: Vec<AppChannel>,
    devices: Vec<Device>,
    mics: Vec<Device>,
    pids: HashMap<u32, Option<process_info::ProcInfo>>,
    mic: Option<IAudioEndpointVolume>,
    mic_id: String,
    version: u64,
}

fn device_id(device: &IMMDevice) -> String {
    unsafe {
        match device.GetId() {
            Ok(p) => {
                let s = p.to_string().unwrap_or_default();
                CoTaskMemFree(Some(p.0 as *const _));
                s
            }
            Err(_) => String::new(),
        }
    }
}

fn device_name(device: &IMMDevice) -> String {
    unsafe {
        device
            .OpenPropertyStore(STGM_READ)
            .and_then(|store| store.GetValue(&PKEY_Device_FriendlyName))
            .map(|v| v.to_string())
            .unwrap_or_default()
    }
}

impl Mixer {
    /// Создавать в потоке, где уже вызван CoInitializeEx.
    pub fn new() -> Self {
        let enumerator: Option<IMMDeviceEnumerator> =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok() };
        let mut mixer = Self {
            enumerator,
            device: None,
            device_id: String::new(),
            device_name: String::new(),
            endpoint: None,
            meter: None,
            manager: None,
            master: Channel { key: "#master".into(), name: "Общая громкость".into(), ..Default::default() },
            apps: Vec::new(),
            devices: Vec::new(),
            mics: Vec::new(),
            pids: HashMap::new(),
            mic: None,
            mic_id: String::new(),
            version: 0,
        };
        mixer.refresh_devices();
        mixer.refresh();
        mixer
    }

    /// Устройство вывода по умолчанию и список устройств.
    pub fn refresh_devices(&mut self) {
        let Some(enumerator) = self.enumerator.clone() else { return };
        unsafe {
            // Своё устройство острова (виртуальный микшер) — если выбрано и подключено
            let (out_id, mic_wanted) = crate::settings::read(|s| (s.island.output_device.clone(), s.island.mic_device.clone()));
            let by_id = |id: &str| -> Option<IMMDevice> {
                if id.is_empty() {
                    return None;
                }
                let w = wide(id);
                enumerator.GetDevice(PCWSTR(w.as_ptr())).ok().filter(|d| d.GetState().map(|st| st == DEVICE_STATE_ACTIVE).unwrap_or(false))
            };
            let current = by_id(&out_id).or_else(|| enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia).ok());
            let id = current.as_ref().map(device_id).unwrap_or_default();
            if id != self.device_id || current.is_none() != self.device.is_none() {
                self.apps.clear();
                self.device_id = id;
                self.device = current;
                self.endpoint = None;
                self.meter = None;
                self.manager = None;
                self.device_name = "Нет устройства вывода".into();
                if let Some(d) = &self.device {
                    self.endpoint = d.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok();
                    self.meter = d.Activate::<IAudioMeterInformation>(CLSCTX_ALL, None).ok();
                    self.manager = d.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None).ok();
                    self.device_name = device_name(d);
                }
            }

            let mut devices = Vec::new();
            if let Ok(collection) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) {
                let count = collection.GetCount().unwrap_or(0);
                for i in 0..count {
                    if let Ok(d) = collection.Item(i) {
                        devices.push(Device { id: device_id(&d), name: device_name(&d) });
                    }
                }
            }
            self.devices = devices;

            let mut mics = Vec::new();
            if let Ok(collection) = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE) {
                let count = collection.GetCount().unwrap_or(0);
                for i in 0..count {
                    if let Ok(d) = collection.Item(i) {
                        mics.push(Device { id: device_id(&d), name: device_name(&d) });
                    }
                }
            }
            self.mics = mics;

            // Микрофон острова или по умолчанию
            let mic = by_id(&mic_wanted).or_else(|| enumerator.GetDefaultAudioEndpoint(eCapture, eCommunications).ok());
            let mic_id = mic.as_ref().map(device_id).unwrap_or_default();
            if mic_id != self.mic_id || mic.is_none() != self.mic.is_none() {
                self.mic_id = mic_id;
                self.mic = mic.and_then(|m| m.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok());
            }
        }
        self.publish();
    }

    /// Пересобирает список приложений со звуком.
    pub fn refresh(&mut self) {
        unsafe {
            if let Some(endpoint) = &self.endpoint {
                let v = endpoint.GetMasterVolumeLevelScalar().unwrap_or(0.0);
                let m = endpoint.GetMute().map(|b| b.as_bool()).unwrap_or(false);
                self.master.volume = (v * 100.0).round();
                self.master.muted = m;
            }
            let Some(manager) = self.manager.clone() else {
                self.apps.clear();
                self.publish();
                return;
            };
            let Ok(list) = manager.GetSessionEnumerator() else {
                self.publish();
                return;
            };
            let count = list.GetCount().unwrap_or(0);
            let mut order: Vec<String> = Vec::new();
            let mut groups: HashMap<String, (Option<process_info::ProcInfo>, Vec<Session>)> = HashMap::new();
            let mut seen_pids = Vec::new();
            for i in 0..count {
                let Ok(control) = list.GetSession(i) else { continue };
                if control.GetState().map(|s| s == AudioSessionStateExpired).unwrap_or(true) {
                    continue;
                }
                let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
                let key;
                let mut info = None;
                if control2.IsSystemSoundsSession() == HRESULT(0) {
                    key = SYSTEM_KEY.to_string();
                } else {
                    let pid = control2.GetProcessId().unwrap_or(0);
                    if pid == 0 {
                        continue;
                    }
                    seen_pids.push(pid);
                    let entry = self.pids.entry(pid).or_insert_with(|| process_info::get(pid)).clone();
                    let Some(pi) = entry else { continue };
                    key = pi.key.clone();
                    info = Some(pi);
                }
                let Ok(volume) = control.cast::<ISimpleAudioVolume>() else { continue };
                let meter = control.cast::<IAudioMeterInformation>().ok();
                let lower = key.to_lowercase();
                if !groups.contains_key(&lower) {
                    order.push(lower.clone());
                    groups.insert(lower.clone(), (info, Vec::new()));
                }
                if let Some(group) = groups.get_mut(&lower) {
                    group.1.push(Session { control, volume, meter });
                }
            }
            self.pids.retain(|pid, _| seen_pids.contains(pid));

            let mut apps: Vec<AppChannel> = Vec::new();
            for key in order {
                let Some((info, sessions)) = groups.remove(&key) else { continue };
                let existing = self.apps.iter().position(|a| a.channel.key.to_lowercase() == key);
                let mut channel = match existing {
                    Some(i) => self.apps[i].channel.clone(),
                    None if key == SYSTEM_KEY => Channel {
                        key: SYSTEM_KEY.into(),
                        name: "Системные звуки".into(),
                        is_system: true,
                        ..Default::default()
                    },
                    None => {
                        let pi = info.clone().unwrap();
                        Channel { key: pi.key, name: pi.name, icon: pi.icon, ..Default::default() }
                    }
                };
                let first = &sessions[0];
                channel.volume = (first.volume.GetMasterVolume().unwrap_or(1.0) * 100.0).round();
                channel.muted = sessions.iter().all(|s| s.volume.GetMute().map(|b| b.as_bool()).unwrap_or(false));
                let app = AppChannel { channel, sessions };
                if app.channel.is_system {
                    apps.insert(0, app);
                } else {
                    apps.push(app);
                }
            }
            self.apps = apps;
        }
        self.publish();
    }

    /// Индикаторы уровня — часто, пока открыт микшер.
    pub fn update_peaks(&mut self) {
        unsafe {
            if let Some(meter) = &self.meter {
                let raw = meter.GetPeakValue().unwrap_or(0.0);
                self.master.peak = decay(self.master.peak, raw);
            }
            for app in &mut self.apps {
                let raw = app
                    .sessions
                    .iter()
                    .filter_map(|s| s.meter.as_ref().and_then(|m| m.GetPeakValue().ok()))
                    .fold(0.0f32, f32::max);
                app.channel.peak = decay(app.channel.peak, raw);
            }
            if let Some(endpoint) = &self.endpoint {
                self.master.volume = (endpoint.GetMasterVolumeLevelScalar().unwrap_or(0.0) * 100.0).round();
                self.master.muted = endpoint.GetMute().map(|b| b.as_bool()).unwrap_or(false);
            }
        }
        self.publish();
    }

    /// Только общая громкость: дёшево, для подсказки в трее и острова.
    pub fn poll_master(&mut self) -> bool {
        unsafe {
            if let Some(endpoint) = &self.endpoint {
                let v = (endpoint.GetMasterVolumeLevelScalar().unwrap_or(0.0) * 100.0).round();
                let m = endpoint.GetMute().map(|b| b.as_bool()).unwrap_or(false);
                let mic = self.mic_muted();
                let snap_mic = snapshot().mic_muted;
                if v != self.master.volume || m != self.master.muted || mic != snap_mic {
                    self.master.volume = v;
                    self.master.muted = m;
                    self.publish();
                    return true;
                }
            }
        }
        false
    }

    fn publish(&mut self) {
        self.version += 1;
        let snap = Snapshot {
            has_device: self.device.is_some(),
            device_id: self.device_id.clone(),
            device_name: self.device_name.clone(),
            master: Channel { name: "Общая громкость".into(), ..self.master.clone() },
            apps: self.apps.iter().map(|a| a.channel.clone()).collect(),
            devices: self.devices.clone(),
            mics: self.mics.clone(),
            mic_available: self.mic.is_some(),
            mic_muted: self.mic_muted(),
            version: self.version,
        };
        if let Ok(mut s) = SNAPSHOT.lock() {
            *s = Some(snap);
        }
    }

    pub fn find_app(&self, key: &str) -> Option<Channel> {
        let lower = key.to_lowercase();
        self.apps.iter().find(|a| a.channel.key.to_lowercase() == lower).map(|a| a.channel.clone())
    }

    pub fn channel(&self, target: &Target) -> Option<Channel> {
        match target {
            Target::Master => self.device.as_ref().map(|_| self.master.clone()),
            Target::App(key) => self.find_app(key),
        }
    }

    pub fn set_volume(&mut self, target: &Target, volume: f32) {
        let volume = volume.round().clamp(0.0, 100.0);
        unsafe {
            match target {
                Target::Master => {
                    if let Some(e) = &self.endpoint {
                        let _ = e.SetMasterVolumeLevelScalar(volume / 100.0, std::ptr::null());
                    }
                    self.master.volume = volume;
                }
                Target::App(key) => {
                    let lower = key.to_lowercase();
                    if let Some(app) = self.apps.iter_mut().find(|a| a.channel.key.to_lowercase() == lower) {
                        for s in &app.sessions {
                            let _ = s.volume.SetMasterVolume(volume / 100.0, std::ptr::null());
                        }
                        app.channel.volume = volume;
                    }
                }
            }
        }
        self.publish();
    }

    pub fn set_mute(&mut self, target: &Target, muted: bool) {
        unsafe {
            match target {
                Target::Master => {
                    if let Some(e) = &self.endpoint {
                        let _ = e.SetMute(muted, std::ptr::null());
                    }
                    self.master.muted = muted;
                }
                Target::App(key) => {
                    let lower = key.to_lowercase();
                    if let Some(app) = self.apps.iter_mut().find(|a| a.channel.key.to_lowercase() == lower) {
                        for s in &app.sessions {
                            let _ = s.volume.SetMute(muted, std::ptr::null());
                        }
                        app.channel.muted = muted;
                    }
                }
            }
        }
        self.publish();
    }

    pub fn set_device(&mut self, id: &str) {
        if id == self.device_id {
            return;
        }
        set_default_device(id);
        self.refresh_devices();
        self.refresh();
    }

    /// Следующее устройство вывода; возвращает его.
    pub fn next_device(&mut self) -> Option<Device> {
        if self.devices.len() < 2 {
            return None;
        }
        let i = self.devices.iter().position(|d| d.id == self.device_id).map(|i| i as isize).unwrap_or(-1);
        let next = self.devices[((i + 1) as usize) % self.devices.len()].clone();
        self.set_device(&next.id);
        Some(next)
    }

    pub fn mic_muted(&self) -> bool {
        unsafe { self.mic.as_ref().and_then(|m| m.GetMute().ok()).map(|b| b.as_bool()).unwrap_or(false) }
    }

    /// Переключает микрофон; возвращает новое состояние (true — выключен) или None, если микрофона нет.
    pub fn toggle_mic(&mut self) -> Option<bool> {
        let mic = self.mic.clone()?;
        let muted = !self.mic_muted();
        unsafe {
            let _ = mic.SetMute(muted, std::ptr::null());
        }
        self.publish();
        Some(muted)
    }
}

fn decay(current: f32, raw: f32) -> f32 {
    let next = raw.max(current * 0.82);
    if next < 0.005 {
        0.0
    } else {
        next
    }
}
