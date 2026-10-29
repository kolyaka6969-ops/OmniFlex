//! Запись экрана в MP4 (H.264 через Media Foundation) или GIF. Кадры снимаются через GDI вместе с указателем мыши;
//! звук не пишется. Запись идёт в своём потоке, остров показывает её время и кнопку остановки.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::RECT;
use windows::Win32::Media::MediaFoundation::*;

use super::capture::Grabber;
use crate::i18n::tr;
use crate::settings::{self, RecordFormat};

static STOP: AtomicBool = AtomicBool::new(false);
static STARTED: Mutex<Option<Instant>> = Mutex::new(None);

/// Время начала идущей записи.
pub fn started() -> Option<Instant> {
    STARTED.lock().ok().and_then(|s| *s)
}

pub fn is_recording() -> bool {
    started().is_some()
}

pub fn stop() {
    STOP.store(true, Ordering::SeqCst);
}

pub fn folder() -> PathBuf {
    let custom = settings::read(|s| s.tools.record_dir.clone());
    if !custom.trim().is_empty() {
        return PathBuf::from(custom);
    }
    super::known_folder(&windows::Win32::UI::Shell::FOLDERID_Videos).join("OmniFlex")
}

/// Начинает запись прямоугольника экрана.
pub fn start(rect: RECT) {
    if is_recording() {
        return;
    }
    let (format, fps, high) = settings::read(|s| (s.tools.record_format, s.tools.record_fps, s.tools.record_high_quality));
    let dir = folder();
    let _ = std::fs::create_dir_all(&dir);
    let name = chrono::Local::now().format("OmniFlex %Y-%m-%d %H-%M-%S").to_string();
    let path = dir.join(format!("{name}.{}", if format == RecordFormat::Gif { "gif" } else { "mp4" }));
    STOP.store(false, Ordering::SeqCst);
    if let Ok(mut s) = STARTED.lock() {
        *s = Some(Instant::now());
    }
    crate::shell::post(crate::island::recording_changed);
    std::thread::Builder::new()
        .name("OmniFlex record".into())
        .spawn(move || {
            let result = match format {
                RecordFormat::Gif => record_gif(rect, &path),
                RecordFormat::Mp4 => unsafe { record_mp4(rect, &path, fps.clamp(15, 60) as u32, high) },
            };
            if let Ok(mut s) = STARTED.lock() {
                *s = None;
            }
            crate::shell::post(move || {
                crate::island::recording_changed();
                match result {
                    Ok(()) => crate::island::card(crate::island::Card::file(
                        tr("Запись сохранена", "Recording saved"),
                        path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                        path.clone(),
                    )),
                    Err(e) => {
                        let _ = std::fs::remove_file(&path);
                        crate::util::log_error(format!("запись экрана: {e}"));
                        crate::island::card(crate::island::Card::error(tr("Запись не удалась", "Recording failed"), e));
                    }
                }
            });
        })
        .ok();
}

/// Ширина и высота для H.264: чётные.
fn even(rect: RECT) -> RECT {
    let w = ((rect.right - rect.left) & !1).max(2);
    let h = ((rect.bottom - rect.top) & !1).max(2);
    RECT { left: rect.left, top: rect.top, right: rect.left + w, bottom: rect.top + h }
}

unsafe fn record_mp4(rect: RECT, path: &std::path::Path, fps: u32, high: bool) -> Result<(), String> {
    let rect = even(rect);
    let mut grabber = Grabber::new(rect).ok_or("не удалось начать захват")?;
    let (w, h) = (grabber.w, grabber.h);
    MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| e.to_string())?;
    let result = (|| -> windows::core::Result<()> {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 2)?;
        let attrs = attrs.ok_or_else(windows::core::Error::empty)?;
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
        let path_w = crate::util::wide(&path.to_string_lossy());
        let writer = MFCreateSinkWriterFromURL(PCWSTR(path_w.as_ptr()), None, &attrs)?;

        let pixels = (w * h) as f32;
        let bitrate = (pixels * fps as f32 * if high { 0.20 } else { 0.09 }) as u32;
        let out = MFCreateMediaType()?;
        out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
        out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate.max(1_000_000))?;
        out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        out.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64)?;
        out.SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1)?;
        out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        let stream = writer.AddStream(&out)?;

        let input = MFCreateMediaType()?;
        input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
        input.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        input.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64)?;
        input.SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1)?;
        input.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        // Кадры сверху вниз — положительный шаг строки
        input.SetUINT32(&MF_MT_DEFAULT_STRIDE, w * 4)?;
        writer.SetInputMediaType(stream, &input, None)?;
        writer.BeginWriting()?;

        let frame_time = Duration::from_secs_f64(1.0 / fps as f64);
        let start = Instant::now();
        let mut next = start;
        let mut last_ts: i64 = -1;
        let len = w * h * 4;
        while !STOP.load(Ordering::SeqCst) {
            let now = Instant::now();
            if now < next {
                std::thread::sleep(next - now);
            }
            next += frame_time;
            // Отстали (тяжёлый кадр) — догоняем, не копя долг
            if Instant::now() > next + frame_time * 2 {
                next = Instant::now() + frame_time;
            }
            let ts = (start.elapsed().as_nanos() / 100) as i64;
            if ts <= last_ts {
                continue;
            }
            let frame = grabber.frame();
            let buffer = MFCreateMemoryBuffer(len)?;
            let mut ptr: *mut u8 = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(frame.as_ptr(), ptr, len as usize);
            buffer.Unlock()?;
            buffer.SetCurrentLength(len)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(ts)?;
            sample.SetSampleDuration((10_000_000 / fps) as i64)?;
            writer.WriteSample(stream, &sample)?;
            last_ts = ts;
        }
        writer.Finalize()?;
        Ok(())
    })();
    let _ = MFShutdown();
    result.map_err(|e| e.message().to_string())
}

fn record_gif(rect: RECT, path: &std::path::Path) -> Result<(), String> {
    use image::codecs::gif::{GifEncoder, Repeat};
    let mut grabber = Grabber::new(rect).ok_or("не удалось начать захват")?;
    let (w, h) = (grabber.w, grabber.h);
    // GIF тяжёлый: не больше 720 точек в ширину и 10 кадров в секунду
    let scale = (720.0 / w as f32).min(1.0);
    let (ow, oh) = (((w as f32 * scale) as u32).max(1), ((h as f32 * scale) as u32).max(1));
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = GifEncoder::new_with_speed(std::io::BufWriter::new(file), 20);
    encoder.set_repeat(Repeat::Infinite).map_err(|e| e.to_string())?;
    let frame_time = Duration::from_millis(100);
    let mut next = Instant::now();
    let mut frames = 0;
    while !STOP.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now < next {
            std::thread::sleep(next - now);
        }
        let shot_at = Instant::now();
        next = shot_at + frame_time;
        let mut rgba = grabber.frame().to_vec();
        for p in rgba.chunks_exact_mut(4) {
            p.swap(0, 2);
            p[3] = 255;
        }
        let img = image::RgbaImage::from_raw(w, h, rgba).ok_or("кадр")?;
        let img = if scale < 1.0 { image::imageops::resize(&img, ow, oh, image::imageops::FilterType::Triangle) } else { img };
        // Задержка кадра — сколько он на самом деле занял, чтобы GIF шёл с настоящей скоростью
        let delay = image::Delay::from_saturating_duration(frame_time.max(shot_at.elapsed()));
        encoder.encode_frame(image::Frame::from_parts(img, 0, 0, delay)).map_err(|e| e.to_string())?;
        frames += 1;
    }
    if frames == 0 {
        return Err("нет кадров".into());
    }
    Ok(())
}
