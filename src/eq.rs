//! Звук по частотам для края-эквалайзера острова: захват того, что играет устройство вывода (loopback WASAPI),
//! и быстрое преобразование Фурье. Поток живёт, только пока остров просит полосы.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator,
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED};

pub const BANDS: usize = 28;
const FFT: usize = 1024;

static RUNNING: AtomicBool = AtomicBool::new(false);
static WANTED: AtomicBool = AtomicBool::new(false);
static LAST_ASK: AtomicU64 = AtomicU64::new(0);
static LEVELS: Mutex<[f32; BANDS]> = Mutex::new([0.0; BANDS]);

/// Текущие полосы 0..1, слева низкие. Заодно держит захват включённым: без вопросов он сам гаснет через пару секунд.
pub fn levels() -> [f32; BANDS] {
    LAST_ASK.store(crate::util::tick_ms(), Ordering::Relaxed);
    if !RUNNING.load(Ordering::SeqCst) {
        start();
    }
    LEVELS.lock().map(|l| *l).unwrap_or([0.0; BANDS])
}

/// Захват больше не нужен (музыку поставили на паузу, эквалайзер выключили).
pub fn stop() {
    WANTED.store(false, Ordering::SeqCst);
}

fn start() {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    WANTED.store(true, Ordering::SeqCst);
    let ok = std::thread::Builder::new()
        .name("OmniFlex eq".into())
        .spawn(|| {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            if let Err(e) = unsafe { run() } {
                crate::util::log_error(format!("эквалайзер: {e}"));
                std::thread::sleep(Duration::from_secs(5));
            }
            if let Ok(mut l) = LEVELS.lock() {
                *l = [0.0; BANDS];
            }
            RUNNING.store(false, Ordering::SeqCst);
        })
        .is_ok();
    if !ok {
        RUNNING.store(false, Ordering::SeqCst);
    }
}

fn keep_going() -> bool {
    WANTED.load(Ordering::SeqCst) && crate::util::tick_ms().saturating_sub(LAST_ASK.load(Ordering::Relaxed)) < 2000
}

unsafe fn run() -> windows::core::Result<()> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let wanted = crate::settings::read(|s| s.island.output_device.clone());
    let device = if wanted.is_empty() {
        enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?
    } else {
        let id = crate::util::wide(&wanted);
        enumerator
            .GetDevice(windows::core::PCWSTR(id.as_ptr()))
            .or_else(|_| enumerator.GetDefaultAudioEndpoint(eRender, eConsole))?
    };
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
    let format = client.GetMixFormat()?;
    let fmt = &*format;
    let channels = fmt.nChannels.max(1) as usize;
    let rate = fmt.nSamplesPerSec.max(8000) as f32;
    let bits = fmt.wBitsPerSample;
    let is_float = is_float_format(fmt);
    client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 200_000, 0, format, None)?;
    CoTaskMemFree(Some(format as *const _));
    let capture: IAudioCaptureClient = client.GetService()?;
    client.Start()?;

    let mut ring: Vec<f32> = vec![0.0; FFT];
    let mut filled = 0usize;
    let mut pos = 0usize;
    let mut smooth = [0.0f32; BANDS];
    let mut peak = 0.05f32;
    let window: Vec<f32> = (0..FFT).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (FFT - 1) as f32).cos()).collect();
    let edges = band_edges(rate);

    while keep_going() {
        std::thread::sleep(Duration::from_millis(15));
        loop {
            let packet = capture.GetNextPacketSize()?;
            if packet == 0 {
                break;
            }
            let mut data = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
            let silent = flags & 0x2 != 0;
            for f in 0..frames as usize {
                let mut v = 0.0f32;
                if !silent && !data.is_null() {
                    for c in 0..channels {
                        let i = f * channels + c;
                        v += if is_float {
                            *(data as *const f32).add(i)
                        } else if bits == 16 {
                            *(data as *const i16).add(i) as f32 / 32768.0
                        } else if bits == 32 {
                            *(data as *const i32).add(i) as f32 / 2147483648.0
                        } else {
                            0.0
                        };
                    }
                }
                ring[pos] = v / channels as f32;
                pos = (pos + 1) % FFT;
                filled = (filled + 1).min(FFT);
            }
            capture.ReleaseBuffer(frames)?;
        }
        if filled < FFT {
            continue;
        }
        let mut re: Vec<f32> = (0..FFT).map(|i| ring[(pos + i) % FFT] * window[i]).collect();
        let mut im = vec![0.0f32; FFT];
        fft(&mut re, &mut im);
        let mut bands = [0.0f32; BANDS];
        for b in 0..BANDS {
            let (lo, hi) = edges[b];
            let mut m = 0.0f32;
            for k in lo..hi.max(lo + 1) {
                m = m.max((re[k] * re[k] + im[k] * im[k]).sqrt());
            }
            bands[b] = (1.0 + m).ln();
        }
        // Громкость подстраивается сама: тихая музыка тоже должна шевелить край
        let frame_max = bands.iter().cloned().fold(0.0f32, f32::max);
        peak = if frame_max > peak { frame_max } else { (peak * 0.995).max(0.05) };
        for b in 0..BANDS {
            let v = (bands[b] / peak).clamp(0.0, 1.0);
            smooth[b] = if v > smooth[b] { smooth[b] * 0.4 + v * 0.6 } else { smooth[b] * 0.85 + v * 0.15 };
        }
        if let Ok(mut l) = LEVELS.lock() {
            *l = smooth;
        }
    }
    let _ = client.Stop();
    Ok(())
}

unsafe fn is_float_format(fmt: &WAVEFORMATEX) -> bool {
    const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
    if fmt.wFormatTag == WAVE_FORMAT_IEEE_FLOAT {
        return true;
    }
    if fmt.wFormatTag == WAVE_FORMAT_EXTENSIBLE {
        let ext = &*(fmt as *const WAVEFORMATEX as *const WAVEFORMATEXTENSIBLE);
        let sub = ext.SubFormat;
        // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: 00000003-0000-0010-8000-00aa00389b71
        return sub.data1 == 3;
    }
    false
}

/// Полосы по логарифмической шкале от 40 Гц до 16 кГц: номера отсчётов БПФ.
fn band_edges(rate: f32) -> Vec<(usize, usize)> {
    let (f0, f1) = (40.0f32, 16000.0f32.min(rate / 2.0 - 1.0));
    let bin = |f: f32| ((f / rate * FFT as f32) as usize).clamp(1, FFT / 2 - 1);
    (0..BANDS)
        .map(|b| {
            let lo = f0 * (f1 / f0).powf(b as f32 / BANDS as f32);
            let hi = f0 * (f1 / f0).powf((b + 1) as f32 / BANDS as f32);
            (bin(lo), bin(hi))
        })
        .collect()
}

/// БПФ по основанию 2, на месте.
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -std::f32::consts::TAU / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let a = start + k;
                let b = a + len / 2;
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = nr;
            }
        }
        len <<= 1;
    }
}
