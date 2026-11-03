//! 4.7b — декодирование видео через Windows Media Foundation.
//!
//! Инфраструктурный модуль (не UI): нужен и миниатюрам (превью-кадр
//! видео в галерее), и просмотрщику (первый кадр при открытии).
//!
//! Почему Media Foundation, а не ffmpeg: системный API Windows 10/11 —
//! НУЛЕВОЙ дополнительный вес exe (крейт windows уже в проекте),
//! аппаратное декодирование H.264/H.265 из коробки; ffmpeg — это
//! ~50–100 МБ DLL, раздувание exe и усиление SmartScreen-проблемы.
//!
//! Почему Source Reader, а не MediaEngine: нам нужны САМИ кадры (пишем
//! в единый viewer-image и строим миниатюры), а MediaEngine рисует в
//! свою поверхность.

/// Видео-расширения, которые умеет декодировать Media Foundation
/// (совпадает со списком сканера).
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "webm", "mov", "mkv", "avi"];

/// Это путь к видео? (по расширению, без учёта регистра)
pub fn is_video(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| VIDEO_EXTENSIONS.iter().any(|&v| e.eq_ignore_ascii_case(v)))
        .unwrap_or(false)
}

/// 4.7b-1 — декодировать первый кадр видео в RGBA.
///
/// Возвращает (данные RGBA, ширина, высота). Ошибки — строкой: вызывающие
/// ветки (миниатюры, просмотрщик) показывают файл как «битый» — обычный
/// путь скипа сработает.
#[cfg(windows)]
pub fn first_frame_rgba(path: &std::path::Path) -> Result<(Vec<u8>, u32, u32), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Media::MediaFoundation::{
        IMFMediaType, IMFSourceReader, MFCreateMediaType, MFCreateSourceReaderFromURL,
        MFMediaType_Video, MFShutdown, MFStartup, MFVideoFormat_RGB32, MFSTARTUP_LITE,
        MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SDK_VERSION,
        MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    };

    let start = std::time::Instant::now();

    // 1) Инициализация MF (LITE — без полного COM, достаточно для
    //    Source Reader). Гвардируем Shutdown при любом выходе (COM-гигиена).
    unsafe {
        MFStartup(MF_SDK_VERSION, MFSTARTUP_LITE).map_err(|e| format!("video: MFStartup: {e}"))?;
    }
    struct MfGuard;
    impl Drop for MfGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = MFShutdown();
            }
        }
    }
    let _mf = MfGuard;

    // 2) Source Reader из пути файла (URL — широкая строка с нулём).
    let url: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let reader: IMFSourceReader = unsafe {
        MFCreateSourceReaderFromURL(PCWSTR(url.as_ptr()), None)
            .map_err(|e| format!("video: открыть «{}»: {e}", path.display()))?
    };

    // 3) Требуем от декодера RGB32 (в памяти — BGRA): простейший формат
    //    для конвертации в RGBA (свап B↔R + альфа=255).
    let media_type: IMFMediaType =
        unsafe { MFCreateMediaType().map_err(|e| format!("video: MFCreateMediaType: {e}"))? };
    unsafe {
        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| format!("video: major type: {e}"))?;
        media_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .map_err(|e| format!("video: subtype RGB32: {e}"))?;
        reader
            .SetCurrentMediaType(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                None,
                &media_type,
            )
            .map_err(|e| format!("video: SetCurrentMediaType: {e}"))?;
    }

    // 4) Читаем сэмплы, пока не придёт кадр с данными (первые сэмплы
    //    могут быть пустыми/служебными — лимит 64 на защиту от зависания).
    let mut frame: Option<(Vec<u8>, u32, u32)> = None;
    for _ in 0..64 {
        let mut sample: Option<windows::Win32::Media::MediaFoundation::IMFSample> = None;
        let mut stream_flags: u32 = 0;
        let mut timestamp: i64 = 0;
        unsafe {
            reader
                .ReadSample(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    0,
                    None,
                    Some(&mut stream_flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
                .map_err(|e| format!("video: ReadSample: {e}"))?;
        }
        let Some(sample) = sample else { continue };

        // Копируем данные буфера (Lock → to_vec → Unlock).
        let buffer = unsafe {
            sample
                .ConvertToContiguousBuffer()
                .map_err(|e| format!("video: буфер: {e}"))?
        };
        let data = unsafe {
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut len: u32 = 0;
            buffer
                .Lock(&mut ptr, None, Some(&mut len))
                .map_err(|e| format!("video: Lock: {e}"))?;
            let owned = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            let _ = buffer.Unlock();
            owned
        };
        if data.is_empty() {
            continue;
        }

        // Размеры кадра — из MF_MT_FRAME_SIZE (u64: старшие 32 = W,
        // младшие 32 = H).
        let (w, h) = unsafe {
            let cur: IMFMediaType = reader
                .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
                .map_err(|e| format!("video: GetCurrentMediaType: {e}"))?;
            let packed = cur
                .GetUINT64(&MF_MT_FRAME_SIZE)
                .map_err(|e| format!("video: MF_MT_FRAME_SIZE: {e}"))?;
            ((packed >> 32) as u32, (packed & 0xFFFF_FFFF) as u32)
        };
        if w == 0 || h == 0 {
            return Err("video: нулевые размеры кадра".to_string());
        }

        frame = Some((data, w, h));
        break;
    }

    let Some((mut rgba, w, h)) = frame else {
        return Err("video: кадры не пришли (64 сэмпла пустые)".to_string());
    };
    // RGB32 = BGRA в памяти: свап B↔R, альфа = 255 (непрозрачный кадр).
    for px in rgba.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
        px[3] = 255;
    }
    log::info!(
        "video: первый кадр {}x{} за {:?} («{}»)",
        w,
        h,
        start.elapsed(),
        path.display()
    );
    Ok((rgba, w, h))
}

#[cfg(test)]
mod tests {
    use super::is_video;
    use std::path::Path;

    #[test]
    fn video_extensions_detected() {
        assert!(is_video(Path::new("C:/a/clip.MP4")));
        assert!(is_video(Path::new("C:/a/clip.mov")));
        assert!(is_video(Path::new("C:/a/clip.webm")));
        assert!(!is_video(Path::new("C:/a/photo.jpg")));
        assert!(!is_video(Path::new("C:/a/photo.png")));
        assert!(!is_video(Path::new("C:/a/noext")));
    }

    /// 4.7b-1 — контракт конвертации BGRA→RGBA (без MF — чистая логика
    /// свапа каналов, инлайн в first_frame_rgba).
    #[test]
    fn bgra_to_rgba_swaps_channels() {
        // BGRA: синий 255, зелёный 128, красный 0, альфа 0 (мусор).
        let mut px = [255u8, 128, 0, 0];
        px.swap(0, 2);
        px[3] = 255;
        // RGBA: красный 255, зелёный 128, синий 0, альфа 255.
        assert_eq!(px, [0, 128, 255, 255]);
    }
}
