//! Голос в текст: первое нажатие начинает слушать, второе — распознаёт. Текст попадает в буфер, а если включено
//! в настройках — сразу в окно, где стоял курсор. Распознаёт Windows (бесплатно, нужен включённый «Распознавание
//! голоса в сети») или Whisper через API из настроек ИИ.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Foundation::TypedEventHandler;
use windows::Media::SpeechRecognition::SpeechRecognizer;
use windows::Win32::Foundation::HWND;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow};

use crate::i18n::tr;
use crate::island::{self, Card};
use crate::settings::{self, VoiceEngine};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static TARGET: AtomicIsize = AtomicIsize::new(0);
static STARTED: Mutex<Option<Instant>> = Mutex::new(None);

pub fn listening_since() -> Option<Instant> {
    if ACTIVE.load(Ordering::SeqCst) && !STOP.load(Ordering::SeqCst) {
        STARTED.lock().ok().and_then(|s| *s)
    } else {
        None
    }
}

/// Начать или закончить диктовку.
pub fn toggle() {
    if ACTIVE.load(Ordering::SeqCst) {
        STOP.store(true, Ordering::SeqCst);
        island::voice_changed();
        return;
    }
    ACTIVE.store(true, Ordering::SeqCst);
    STOP.store(false, Ordering::SeqCst);
    if let Ok(mut s) = STARTED.lock() {
        *s = Some(Instant::now());
    }
    TARGET.store(unsafe { GetForegroundWindow().0 as isize }, Ordering::SeqCst);
    let engine = settings::read(|s| s.voice.engine);
    island::voice_changed();
    std::thread::Builder::new()
        .name("OmniFlex voice".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let result = match engine {
                VoiceEngine::Windows => windows_dictation(),
                VoiceEngine::Api => record_wav().and_then(|wav| {
                    crate::shell::post(|| island::card(Card::busy(tr("Распознаю речь…", "Recognizing speech…"))));
                    crate::media::runtime().block_on(crate::ai::transcribe(wav))
                }),
            };
            ACTIVE.store(false, Ordering::SeqCst);
            crate::shell::post(move || {
                island::voice_changed();
                finished(result);
            });
        })
        .ok();
}

fn finished(result: Result<String, String>) {
    match result {
        Ok(text) if !text.trim().is_empty() => {
            let text = text.trim().to_string();
            crate::tools::clipboard::set_text(&text);
            if settings::read(|s| s.voice.paste) {
                let target = HWND(TARGET.load(Ordering::SeqCst) as _);
                unsafe {
                    let _ = SetForegroundWindow(target);
                }
                crate::tools::clipboard::send_paste();
            }
            island::card(Card::text(tr("Сказанное скопировано", "Speech copied"), text));
        }
        Ok(_) => island::card(Card::error(tr("Голос в текст", "Voice to text"), tr("Ничего не расслышал", "Heard nothing").into())),
        Err(e) => island::card(Card::error(tr("Голос в текст", "Voice to text"), e)),
    }
}

/// Распознавание речи Windows: непрерывная диктовка, пока не нажмут ещё раз.
fn windows_dictation() -> Result<String, String> {
    let explain = |e: windows::core::Error| -> String {
        // 0x80045509: не принято соглашение о распознавании речи в сети
        if e.code().0 as u32 == 0x8004_5509 {
            tr(
                "Включите «Распознавание голоса в сети» в Параметрах Windows → Конфиденциальность → Голосовые функции, или выберите Whisper в настройках",
                "Turn on Online speech recognition in Windows Settings → Privacy → Speech, or choose Whisper in settings",
            )
            .to_string()
        } else {
            e.message().to_string()
        }
    };
    let recognizer = SpeechRecognizer::new().map_err(explain)?;
    let compiled = recognizer.CompileConstraintsAsync().and_then(|op| op.get()).map_err(explain)?;
    let status = compiled.Status().map_err(explain)?;
    if status.0 != 0 {
        return Err(format!("{}: {}", tr("Распознавание недоступно", "Recognition unavailable"), status.0));
    }
    let session = recognizer.ContinuousRecognitionSession().map_err(explain)?;
    let _ = session.SetAutoStopSilenceTimeout(windows::Foundation::TimeSpan { Duration: 600 * 10_000_000 });
    let parts: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = parts.clone();
    let token = session
        .ResultGenerated(&TypedEventHandler::new(move |_, args: &Option<windows::Media::SpeechRecognition::SpeechContinuousRecognitionResultGeneratedEventArgs>| {
            if let Some(args) = args {
                if let Ok(text) = args.Result().and_then(|r| r.Text()) {
                    let t = text.to_string();
                    if !t.trim().is_empty() {
                        if let Ok(mut p) = sink.lock() {
                            p.push(t);
                        }
                    }
                }
            }
            Ok(())
        }))
        .map_err(explain)?;
    session.StartAsync().and_then(|op| op.get()).map_err(explain)?;
    while !STOP.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = session.StopAsync().and_then(|op| op.get());
    let _ = session.RemoveResultGenerated(token);
    let text = parts.lock().map(|p| p.join(" ")).unwrap_or_default();
    Ok(text)
}

unsafe fn is_float(fmt: &WAVEFORMATEX) -> bool {
    if fmt.wFormatTag == 3 {
        return true;
    }
    if fmt.wFormatTag == 0xFFFE {
        let ext = &*(fmt as *const WAVEFORMATEX as *const WAVEFORMATEXTENSIBLE);
        return ext.SubFormat.data1 == 3;
    }
    false
}

/// Запись микрофона до второго нажатия: WAV 16 кГц моно.
fn record_wav() -> Result<Vec<u8>, String> {
    unsafe {
        let run = || -> windows::core::Result<(Vec<f32>, u32)> {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let wanted = settings::read(|s| s.island.mic_device.clone());
            let device = if wanted.is_empty() {
                enumerator.GetDefaultAudioEndpoint(eCapture, eConsole)?
            } else {
                let id = crate::util::wide(&wanted);
                enumerator.GetDevice(windows::core::PCWSTR(id.as_ptr())).or_else(|_| enumerator.GetDefaultAudioEndpoint(eCapture, eConsole))?
            };
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let format = client.GetMixFormat()?;
            let fmt = &*format;
            let (channels, rate, bits, float) = (fmt.nChannels.max(1) as usize, fmt.nSamplesPerSec, fmt.wBitsPerSample, is_float(fmt));
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, 0, 10_000_000, 0, format, None)?;
            CoTaskMemFree(Some(format as *const _));
            let capture: IAudioCaptureClient = client.GetService()?;
            client.Start()?;
            let mut samples = Vec::new();
            let limit = Instant::now() + Duration::from_secs(300);
            while !STOP.load(Ordering::SeqCst) && Instant::now() < limit {
                std::thread::sleep(Duration::from_millis(20));
                loop {
                    if capture.GetNextPacketSize()? == 0 {
                        break;
                    }
                    let mut data = std::ptr::null_mut();
                    let mut frames = 0u32;
                    let mut flags = 0u32;
                    capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                    for f in 0..frames as usize {
                        let mut v = 0.0f32;
                        if flags & 0x2 == 0 && !data.is_null() {
                            for c in 0..channels {
                                let i = f * channels + c;
                                v += if float {
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
                        samples.push(v / channels as f32);
                    }
                    capture.ReleaseBuffer(frames)?;
                }
            }
            let _ = client.Stop();
            Ok((samples, rate))
        };
        let (samples, rate) = run().map_err(|e| e.message().to_string())?;
        if samples.len() < (rate as usize) / 4 {
            return Err(tr("Слишком коротко", "Too short").into());
        }
        Ok(wav_16k(&samples, rate))
    }
}

fn wav_16k(samples: &[f32], rate: u32) -> Vec<u8> {
    let out_rate = 16000u32;
    let ratio = rate as f64 / out_rate as f64;
    let n = (samples.len() as f64 / ratio) as usize;
    let mut pcm = Vec::with_capacity(n * 2);
    for i in 0..n {
        let pos = i as f64 * ratio;
        let j = pos as usize;
        let frac = (pos - j as f64) as f32;
        let a = samples[j.min(samples.len() - 1)];
        let b = samples[(j + 1).min(samples.len() - 1)];
        let v = ((a + (b - a) * frac).clamp(-1.0, 1.0) * 32767.0) as i16;
        pcm.extend_from_slice(&v.to_le_bytes());
    }
    let mut wav = Vec::with_capacity(44 + pcm.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&out_rate.to_le_bytes());
    wav.extend_from_slice(&(out_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
    wav.extend_from_slice(&pcm);
    wav
}
