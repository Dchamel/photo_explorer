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

/// 4.7f — убрать паддинг строк кадра: Media Foundation отдаёт строку
/// длиной stride ≥ w*4 (выравнивание по границе). Копируем ровно w*4
/// байт с каждой строки — иначе w*h*4 != len буфера и миниатюра падает
/// («буфер кадра не сошёлся»). None — буфер меньше h*stride (битый кадр).
fn strip_stride(data: &[u8], stride: usize, w: usize, h: usize) -> Option<Vec<u8>> {
    let row = w * 4;
    if stride == row {
        return Some(data.to_vec()); // выравнивание не добавило паддинга
    }
    if stride < row || data.len() < h * stride {
        return None;
    }
    let mut out = Vec::with_capacity(h * row);
    for y in 0..h {
        let start = y * stride;
        out.extend_from_slice(&data[start..start + row]);
    }
    Some(out)
}

/// 4.7f — заглушка кадра видео (тёмная карточка 96x96 с белым
/// треугольником «play»), когда декод не удался (нет кодека в системе,
/// битый файл): файл НЕ теряется — и галерея, и просмотрщик показывают
/// заглушку, файл видно и можно открыть/листать.
pub fn video_placeholder() -> (Vec<u8>, u32, u32) {
    const SIZE: u32 = 96;
    // Треугольник «play» (вершины в сетке 96x96, центр кадра).
    let (ax, ay) = (38.0f32, 30.0f32);
    let (bx, by) = (38.0f32, 66.0f32);
    let (cx, cy) = (66.0f32, 48.0f32);
    let mut data = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // Внутренность треугольника: знаковые площади к трём рёбрам
            // без смешанных знаков (все положительные или все отрицательные).
            let d1 = (bx - ax) * (py - ay) - (by - ay) * (px - ax);
            let d2 = (cx - bx) * (py - by) - (cy - by) * (px - bx);
            let d3 = (ax - cx) * (py - cy) - (ay - cy) * (px - cx);
            let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
            let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
            let (r, g, b) = if !(has_neg && has_pos) {
                (245u8, 245, 245) // треугольник — светлый
            } else {
                (38u8, 38, 46) // фон — тёмный
            };
            data.extend_from_slice(&[r, g, b, 255]);
        }
    }
    (data, SIZE, SIZE)
}

/// 4.7h — заглушка БИТОГО файла (фото, чей декод провалился): тёмная
/// карточка с серым «крестом-поломанной картинкой». ОТЛИЧАЕТСЯ от
/// видео-треугольника осознанно: раньше битые фото получали тот же
/// треугольник «play» — заказчик кликал по ним как по видео, а там
/// скип битого. Теперь по значку видно: треугольник = видео, крест =
/// файл сломан.
pub fn broken_placeholder() -> (Vec<u8>, u32, u32) {
    const SIZE: u32 = 96;
    let mut data = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            // Крест: две диагонали толщиной ~6px в центре 96x96.
            let d1 = (x as i32 - y as i32).abs();
            let d2 = (x as i32 + y as i32 - SIZE as i32 + 1).abs();
            let (r, g, b) = if d1 <= 3 || d2 <= 3 {
                (120u8, 120, 126) // серый крест
            } else {
                (26u8, 26, 30) // почти чёрный фон
            };
            data.extend_from_slice(&[r, g, b, 255]);
        }
    }
    (data, SIZE, SIZE)
}

/// 4.7b-1 — декодировать первый кадр видео в RGBA.
///
/// Возвращает (данные RGBA, ширина, высота). Ошибки — строкой: вызывающие
/// ветки (миниатюры, просмотрщик) показывают файл как «битый» — обычный
/// путь скипа сработает.
/// 4.7g — MF инициализируется ОДИН РАЗ на процесс (OnceLock): раньше
/// MFStartup/MFShutdown гонялись на КАЖДОМ видео (281 файл × инициализация
/// COM-инфраструктуры = минуты). MFStartup референс-счётный — повторные
/// вызовы безвредны, но и не нужны; Shutdown не делаем — ОС почистит.
#[cfg(windows)]
fn mf_ensure_started() -> Result<(), String> {
    use std::sync::OnceLock;
    use windows::Win32::Media::MediaFoundation::{MFStartup, MFSTARTUP_LITE, MF_SDK_VERSION};
    static MF_STARTED: OnceLock<Result<(), String>> = OnceLock::new();
    let started = MF_STARTED.get_or_init(|| unsafe {
        MFStartup(MF_SDK_VERSION, MFSTARTUP_LITE).map_err(|e| format!("video: MFStartup: {e}"))
    });
    started.clone()
}

#[cfg(windows)]
pub fn first_frame_rgba(path: &std::path::Path) -> Result<(Vec<u8>, u32, u32), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Media::MediaFoundation::{
        IMFAttributes, IMFMediaType, IMFSourceReader, MFCreateAttributes, MFCreateMediaType,
        MFCreateSourceReaderFromURL, MFMediaType_Video, MFVideoFormat_RGB32, MF_MT_DEFAULT_STRIDE,
        MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
        MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    };

    let start = std::time::Instant::now();

    // 1) Инициализация MF (LITE) — 4.7g: ОДИН РАЗ на процесс, а не на
    //    каждый файл (281 видео × MFStartup = минуты лишнего старта).
    //    MFStartup — референс-счётный и потокобезопасный; Shutdown не
    //    делаем (процесс живёт — MF живёт, ОС почистит на выходе).
    mf_ensure_started()?;

    // 2) Source Reader из пути файла (URL — широкая строка с нулём).
    //    4.7f — ВКЛЮЧАЕМ ВИДЕО-ПРОЦЕССОР: без него Source Reader НЕ
    //    конвертирует нативный формат декодера (NV12 у H.264/H.265) в
    //    запрошенный ниже RGB32 — SetCurrentMediaType падает на любом
    //    реальном видео, ВСЕ миниатюры проваливаются, а галерея молча
    //    выбрасывает файлы (корень «видео не видно в галерее»).
    let url: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let attrs = unsafe {
        let mut attrs_opt: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs_opt, 0)
            .map_err(|e| format!("video: MFCreateAttributes: {e}"))?;
        let attrs = attrs_opt.ok_or_else(|| "video: MFCreateAttributes: пусто".to_string())?;
        attrs
            .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .map_err(|e| format!("video: включить видео-процессор: {e}"))?;
        attrs
    };
    let reader: IMFSourceReader = unsafe {
        MFCreateSourceReaderFromURL(PCWSTR(url.as_ptr()), Some(&attrs))
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
        // младшие 32 = H). Заодно stride (4.7f): длина строки в памяти
        // может быть БОЛЬШЕ w*4 из-за выравнивания.
        let (w, h, stride) = unsafe {
            let cur: IMFMediaType = reader
                .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
                .map_err(|e| format!("video: GetCurrentMediaType: {e}"))?;
            let packed = cur
                .GetUINT64(&MF_MT_FRAME_SIZE)
                .map_err(|e| format!("video: MF_MT_FRAME_SIZE: {e}"))?;
            let w = (packed >> 32) as u32;
            let h = (packed & 0xFFFF_FFFF) as u32;
            // Stride: MF_MT_DEFAULT_STRIDE может отсутствовать — тогда
            // вычисляем из длины буфера (ровно h строк по stride байт).
            // Отрицательный stride (перевёрнутый кадр) приходит огромным
            // u32 — такой случай не трактуем как длину строки.
            let stride = match cur.GetUINT32(&MF_MT_DEFAULT_STRIDE) {
                Ok(s) if s != 0 && (s as i32) > 0 => s as usize,
                _ => data.len() / h.max(1) as usize,
            };
            (w, h, stride)
        };
        if w == 0 || h == 0 {
            return Err("video: нулевые размеры кадра".to_string());
        }

        // 4.7f — режем паддинг построчно: без этого w*h*4 != len и
        // миниатюра падает на «буфер кадра не сошёлся».
        let data = strip_stride(&data, stride, w as usize, h as usize).unwrap_or(data);

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
    use super::{is_video, strip_stride};
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

    /// 4.7f — буфер со stride > w*4: паддинг каждой строки отбрасывается,
    /// на выходе ровно w*h*4 байт настоящих пикселей.
    #[test]
    fn stride_padding_is_stripped() {
        // 2 пикселя в ширину, 2 строки, stride 12 (паддинг 4 байта).
        let data = [
            1u8, 2, 3, 255, 5, 6, 7, 255, 99, 99, 99, 99, //
            9, 10, 11, 255, 13, 14, 15, 255, 88, 88, 88, 88,
        ];
        let out = strip_stride(&data, 12, 2, 2).expect("паддинг режется");
        assert_eq!(
            out,
            [1u8, 2, 3, 255, 5, 6, 7, 255, 9, 10, 11, 255, 13, 14, 15, 255]
        );
    }

    /// 4.7f — stride ровно w*4: паддинга нет, данные проходят как есть.
    #[test]
    fn stride_equal_row_is_passthrough() {
        let data = [1u8, 2, 3, 255, 4, 5, 6, 255];
        let out = strip_stride(&data, 8, 2, 1).expect("без паддинга");
        assert_eq!(out, data);
    }

    /// 4.7f — защита: stride меньше w*4 или буфер короче h*stride → None
    /// (битый кадр не превращаем в мусорную картинку).
    #[test]
    fn stride_broken_buffer_is_none() {
        assert!(strip_stride(&[0u8; 8], 4, 2, 1).is_none()); // stride < w*4
        assert!(strip_stride(&[0u8; 8], 16, 2, 2).is_none()); // буфера мало
    }

    /// 4.7f — заглушка: размер ровно 96*96*4, альфа всюду 255,
    /// центр светлый (треугольник), угол тёмный (фон).
    #[test]
    fn placeholder_is_opaque_card() {
        let (data, w, h) = super::video_placeholder();
        assert_eq!((w, h), (96, 96));
        assert_eq!(data.len(), 96 * 96 * 4);
        assert!(data.as_chunks::<4>().0.iter().all(|px| px[3] == 255));
        let at = |x: usize, y: usize| {
            let i = (y * 96 + x) * 4;
            (data[i], data[i + 1], data[i + 2])
        };
        assert_eq!(at(48, 48), (245, 245, 245)); // центр — треугольник
        assert_eq!(at(4, 4), (38, 38, 46)); // угол — фон
    }

    /// 4.7h — заглушка битого: непрозрачная, центр — серый крест
    /// (диагонали), угол — тёмный фон; ОТЛИЧАЕТСЯ от видео-треугольника.
    #[test]
    fn broken_placeholder_is_cross_not_play() {
        let (data, w, h) = super::broken_placeholder();
        assert_eq!((w, h), (96, 96));
        assert_eq!(data.len(), 96 * 96 * 4);
        assert!(data.as_chunks::<4>().0.iter().all(|px| px[3] == 255));
        let at = |x: usize, y: usize| {
            let i = (y * 96 + x) * 4;
            (data[i], data[i + 1], data[i + 2])
        };
        // Центр на диагонали креста — серый.
        assert_eq!(at(48, 48), (120, 120, 126));
        // Точка ВНЕ диагоналей — тёмный фон (не видео-фон 38,38,46 —
        // другая палитра). (4,4) не годится: это сама диагональ d1=0.
        assert_eq!(at(4, 20), (26, 26, 30));
        // Отличие от видео-заглушки: центр видео — светлый треугольник.
        let (vdata, _, _) = super::video_placeholder();
        let vi = (48usize * 96 + 48) * 4;
        assert_ne!(
            (data[48 * 96 * 4 + 48 * 4], data[48 * 96 * 4 + 48 * 4 + 1]),
            (vdata[vi], vdata[vi + 1])
        );
    }
}
