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
//!
//! 4.7b-2 — ПОТОКОВЫЙ декодер `VideoStream`: кадры читаются по одному
//! (ReadSample), НЕ все сразу (4K-видео целиком = гигабайты RGBA).
//! Ридер живёт в потоке аниматора; UI получает кадр за кадром.

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

/// 4.7L-2b — декодирование картинки ПО СИГНАТУРЕ, не по расширению.
///
/// `image::open` выбирает декодер по расширению файла: webp, названный
/// `.jpg`, получал JPEG-декодер и падал («Illegal start bytes:5249») —
/// 2117 целых файлов в проекте заказчика числились «битыми». Здесь
/// `with_guessed_format()` читает магические байты и ставит ПРАВИЛЬНЫЙ
/// декодер независимо от расширения (webp-в-jpg, png-в-jpg и т.п.).
///
/// Единая точка правды для миниатюр и просмотрщика (DRY): оба зовут
/// этот хелпер. Ошибки — строкой (стиль модуля).
pub fn decode_image_by_signature(path: &std::path::Path) -> Result<image::DynamicImage, String> {
    let reader = image::ImageReader::open(path).map_err(|e| e.to_string())?;
    let reader = reader.with_guessed_format().map_err(|e| e.to_string())?;
    reader.decode().map_err(|e| e.to_string())
}

/// 4.7L-2-diag — классификация «битых» ПО СИГНАТУРЕ (первые байты файла),
/// НЕ по расширению: расширение часто врёт (.jpg с webp внутри давал
/// ложных «битых»). Ярлык — НЕ приговор «файл сломан»: неподдержанные,
/// но ЦЕЛЫЕ форматы (tiff/raw/avif/ico/…) — кандидаты в будущую стадию
/// «доп. форматы»; «неизвестно» — вот это реально повреждённые или мусор.
///
/// Корзины: jpeg/png/gif/bmp (image обязан уметь — упали = повреждены
/// внутри), webp (image умеет — чинится декодом по сигнатуре),
/// видео-контейнеры (avi/mp4/mov/mkv/webm — путь Media Foundation),
/// tiff/raw (TIFF-магия II*/MM0* — и обычный tiff, и большинство RAW:
/// CR2/NEF/ARW/DNG/ORF/PEF…; плюс ftyp+crx = CR3, FUJIFILMCCD = RAF),
/// avif/heic, ico, psd, jxl, wav, «неизвестно».
pub fn sniff_format_label(path: &std::path::Path) -> &'static str {
    let mut head = [0u8; 16];
    match std::fs::File::open(path) {
        Ok(mut f) => {
            use std::io::Read;
            let n = f.read(&mut head).unwrap_or(0);
            sniff_bytes(&head[..n])
        }
        // Файл не открывается вовсе (удалён/права) — отдельная корзина,
        // не путать с «неизвестно» (открылся, но сигнатура не опознана).
        Err(_) => "не читается",
    }
}

/// Чистая часть [`sniff_format_label`]: ярлык по первым байтам буфера.
/// Отдельная функция — тестируется без файлов (KISS).
fn sniff_bytes(head: &[u8]) -> &'static str {
    // Картинки, которые image обязан уметь: декод на них упал —
    // файл повреждён ВНУТРИ (сигнатура целая, данные нет).
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "jpeg";
    }
    if head.starts_with(&[0x89, b'P', b'N', b'G']) {
        return "png";
    }
    if head.starts_with(b"GIF8") {
        return "gif";
    }
    if head.starts_with(b"BM") {
        return "bmp";
    }
    // RIFF-контейнер: подтип в байтах 8..12 (WEBP — картинка, AVI — видео).
    if head.starts_with(b"RIFF") {
        if head.len() >= 12 {
            let sub = &head[8..12];
            if sub == b"WEBP" {
                return "webp";
            }
            if sub == b"AVI " {
                return "видео (avi)";
            }
            if sub == b"WAVE" {
                return "wav";
            }
        }
        return "riff-иное";
    }
    // EBML — контейнер mkv/webm.
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return "видео (mkv/webm)";
    }
    // ISO-BMFF: байты 4..8 = "ftyp", байты 8..12 — бренд.
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        let brand = &head[8..12];
        if brand == b"crx " {
            return "raw (cr3)"; // Canon CR3 — RAW на контейнере BMFF
        }
        if brand == b"avif" || brand == b"avis" {
            return "avif";
        }
        if brand == b"heic"
            || brand == b"heix"
            || brand == b"hevc"
            || brand == b"hevx"
            || brand == b"heim"
            || brand == b"heis"
            || brand == b"mif1"
            || brand == b"msf1"
        {
            return "heic";
        }
        if brand == b"qt  " {
            return "видео (mov)";
        }
        return "видео (mp4)"; // isom/iso2/mp41/mp42/avc1/… — mp4-семейство
    }
    // TIFF-магия: и обычный tiff, и ПОДАВЛЯЮЩЕЕ большинство RAW
    // (CR2/NEF/NRW/ARW/DNG/ORF/PEF/RW2/SRW построены на TIFF).
    if head.starts_with(&[0x49, 0x49, 0x2A, 0x00]) || head.starts_with(&[0x4D, 0x4D, 0x00, 0x2A]) {
        return "tiff/raw";
    }
    if head.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        return "ico";
    }
    if head.starts_with(b"8BPS") {
        return "psd";
    }
    // JPEG XL: контейнер (…JXL ) или чистый codestream FF 0A.
    if head.starts_with(&[0x00, 0x00, 0x00, 0x0C, b'J', b'X', b'L', b' '])
        || head.starts_with(&[0xFF, 0x0A])
    {
        return "jxl";
    }
    // Fujifilm RAF — собственная магия (не TIFF).
    if head.starts_with(b"FUJIFILMCCD-RAW") {
        return "raw (raf)";
    }
    "неизвестно"
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

/// 4.7b-2 — ПОТОКОВЫЙ декодер видео: ридер Media Foundation живёт в
/// структуре, кадры читаются по одному (`next_frame`). НЕ декодирует
/// всё видео в память (4K = гигабайты) — аниматор читает кадр за кадром
/// и спит между ними по таймингу сэмпла.
#[cfg(windows)]
pub struct VideoStream {
    reader: windows::Win32::Media::MediaFoundation::IMFSourceReader,
    /// Ширина/высота кадра (из MF_MT_FRAME_SIZE).
    w: u32,
    h: u32,
    /// Длительность в 100-наносекундных единицах.
    duration_100ns: i64,
}

/// Один кадр видеоряда: RGBA-пиксели + позиция от начала видео
/// (100-наносекундные единицы) — аниматор спит до этого момента.
#[cfg(windows)]
pub struct VideoFrame {
    pub data: Vec<u8>,
    pub w: u32,
    pub h: u32,
    pub time_100ns: i64,
}

#[cfg(windows)]
impl VideoStream {
    /// Открыть видео и подготовить ридер к чтению кадров (RGB32).
    /// Видео-процессор включён (NV12→RGB32), MF уже стартовал (OnceLock).
    pub fn open(path: &std::path::Path) -> Result<Self, String> {
        use windows::core::PCWSTR;
        use windows::Win32::Media::MediaFoundation::{
            IMFMediaType, MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromURL,
            MFMediaType_Video, MFVideoFormat_RGB32, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
            MF_MT_SUBTYPE, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
            MF_SOURCE_READER_FIRST_VIDEO_STREAM,
        };
        mf_ensure_started()?;
        let url: Vec<u16> = path
            .as_os_str()
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let attrs = unsafe {
            let mut attrs_opt: Option<windows::Win32::Media::MediaFoundation::IMFAttributes> = None;
            MFCreateAttributes(&mut attrs_opt, 0)
                .map_err(|e| format!("video: MFCreateAttributes: {e}"))?;
            let attrs = attrs_opt.ok_or_else(|| "video: MFCreateAttributes: пусто".to_string())?;
            attrs
                .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
                .map_err(|e| format!("video: включить видео-процессор: {e}"))?;
            attrs
        };
        let reader = unsafe {
            MFCreateSourceReaderFromURL(PCWSTR(url.as_ptr()), Some(&attrs))
                .map_err(|e| format!("video: открыть «{}»: {e}", path.display()))?
        };
        // Тот же контракт, что у first_frame_rgba: RGB32 (BGRA в памяти).
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
        // Размеры кадра + длительность (бегунок перемотки).
        let (w, h, duration_100ns) = unsafe {
            let cur: IMFMediaType = reader
                .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
                .map_err(|e| format!("video: GetCurrentMediaType: {e}"))?;
            let packed = cur
                .GetUINT64(&MF_MT_FRAME_SIZE)
                .map_err(|e| format!("video: MF_MT_FRAME_SIZE: {e}"))?;
            // 4.7j — длительность МГНОВЕННО из метаданных контейнера:
            // GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE,
            // MF_PD_DURATION) → PROPVARIANT (i64, 100-наносекунды).
            // Фичи Win32_System_Com_StructuredStorage + Win32_System_Variant
            // включены в Cargo.toml — метод доступен.
            let duration = {
                use windows::Win32::Media::MediaFoundation::{
                    MF_PD_DURATION, MF_SOURCE_READER_MEDIASOURCE,
                };
                reader
                    .GetPresentationAttribute(
                        MF_SOURCE_READER_MEDIASOURCE.0 as u32,
                        &MF_PD_DURATION,
                    )
                    .ok()
                    .and_then(|vt| {
                        // hVal на этом уровне раскрытия — сам i64.
                        let v = vt.Anonymous.Anonymous.Anonymous.hVal;
                        (v > 0).then_some(v)
                    })
                    .unwrap_or(0)
            };
            (
                (packed >> 32) as u32,
                (packed & 0xFFFF_FFFF) as u32,
                duration,
            )
        };
        if w == 0 || h == 0 {
            return Err("video: нулевые размеры кадра".to_string());
        }
        Ok(Self {
            reader,
            w,
            h,
            duration_100ns,
        })
    }

    /// Размеры кадра (для UI: пропорции, contain-fit).
    pub fn size(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    /// Длительность видео в 100-наносекундных единицах (бегунок).
    pub fn duration_100ns(&self) -> i64 {
        self.duration_100ns
    }

    /// 4.7b-4 — перемотка на позицию (100-наносекундные единицы).
    ///
    /// Быстрый прыжок через `SetCurrentPosition`: Media Foundation сам
    /// прыгает к БЛИЖАЙШЕМУ КЛЮЧЕВОМУ КАДРУ за миллисекунды, в любую
    /// сторону, БЕЗ декодирования промежуточных кадров.
    ///
    /// Прежний путь (4.7b-2-fix) проматывал кадры по одному: назад не
    /// работал вовсе (цель позади), вперёд — декод всех промежуточных
    /// кадров = секунды заморозки, картинка не обновлялась (кадры
    /// выбрасывались), двигалось только время на шкале.
    ///
    /// Формат времени — 100-нс единицы (VT_I8 PROPVARIANT, GUID_NULL =
    /// «позиция в 100 нс»); MF встаёт на ближайший keyframe ≤ цели,
    /// следующий `next_frame()` вернёт кадр оттуда.
    pub fn seek_100ns(&mut self, pos_100ns: i64) -> Result<(), String> {
        use windows::core::GUID;
        use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
        let pos = pos_100ns.max(0);
        let var = PROPVARIANT::from(pos);
        // GUID_NULL (все нули) = «позиция в 100-наносекундных единицах».
        unsafe {
            self.reader
                .SetCurrentPosition(&GUID::zeroed(), &var)
                .map_err(|e| format!("video: SetCurrentPosition: {e}"))?;
        }
        Ok(())
    }

    /// Прочитать СЛЕДУЮЩИЙ кадр. None — видео закончилось (конец потока).
    /// Кадр: RGBA (свап B→R сделан), паддинг stride отрезан.
    pub fn next_frame(&mut self) -> Result<Option<VideoFrame>, String> {
        use windows::Win32::Media::MediaFoundation::{
            MF_SOURCE_READERF_ENDOFSTREAM, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
        };
        loop {
            let mut sample: Option<windows::Win32::Media::MediaFoundation::IMFSample> = None;
            let mut flags: u32 = 0;
            let mut timestamp: i64 = 0;
            unsafe {
                self.reader
                    .ReadSample(
                        MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                        0,
                        None,
                        Some(&mut flags),
                        Some(&mut timestamp),
                        Some(&mut sample),
                    )
                    .map_err(|e| format!("video: ReadSample: {e}"))?;
            }
            if flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0 {
                return Ok(None); // конец видео
            }
            let Some(sample) = sample else { continue };
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
            // Stride: длина строки в памяти ≥ w*4 (выравнивание).
            let stride = match unsafe {
                self.reader
                    .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
            } {
                Ok(cur) => unsafe {
                    match cur
                        .GetUINT32(&windows::Win32::Media::MediaFoundation::MF_MT_DEFAULT_STRIDE)
                    {
                        Ok(s) if s != 0 && (s as i32) > 0 => s as usize,
                        _ => data.len() / self.h.max(1) as usize,
                    }
                },
                Err(_) => data.len() / self.h.max(1) as usize,
            };
            let Some(mut rgba) = strip_stride(&data, stride, self.w as usize, self.h as usize)
            else {
                return Err("video: буфер кадра не сошёлся со stride".to_string());
            };
            for px in rgba.as_chunks_mut::<4>().0 {
                px.swap(0, 2); // BGRA → RGBA
                px[3] = 255;
            }
            return Ok(Some(VideoFrame {
                data: rgba,
                w: self.w,
                h: self.h,
                time_100ns: timestamp,
            }));
        }
    }
}

// 4.7j — старый путь длительности (прогон кадров + кэш .dur) УДАЛЁН:
// GetPresentationAttribute(MF_PD_DURATION) читает длительность из
// метаданных контейнера МГНОВЕННО (как все плееры), без декода.
// Прежний прогон либо блокировал старт (13 сек), либо не успевал
// параллельно — «0:00 в конце шкалы».

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

    /// 4.7L-2-diag — сигнатуры картинок, которые image обязан уметь:
    /// jpeg/png/gif/bmp (упал декод на них — файл повреждён внутри).
    #[test]
    fn sniff_known_image_signatures() {
        assert_eq!(super::sniff_bytes(&[0xFF, 0xD8, 0xFF, 0xE0]), "jpeg");
        assert_eq!(super::sniff_bytes(&[0x89, b'P', b'N', b'G', 0x0D]), "png");
        assert_eq!(super::sniff_bytes(b"GIF89a"), "gif");
        assert_eq!(super::sniff_bytes(b"BM\x36\x00"), "bmp");
    }

    /// 4.7L-2-diag — RIFF-подтипы по байтам 8..12: webp (картинка) и
    /// avi (видео) различаются ТОЛЬКО ими — по первым байтам не отличить.
    #[test]
    fn sniff_riff_subtypes() {
        let webp = b"RIFF\x24\x00\x00\x00WEBPVP8 ";
        assert_eq!(super::sniff_bytes(webp), "webp");
        let avi = b"RIFF\x24\x00\x00\x00AVI LIST";
        assert_eq!(super::sniff_bytes(avi), "видео (avi)");
        let wav = b"RIFF\x24\x00\x00\x00WAVEfmt";
        assert_eq!(super::sniff_bytes(wav), "wav");
        // Обрезанный RIFF (короче 12 байт) — не гадаем о подтипе.
        assert_eq!(super::sniff_bytes(b"RIFF"), "riff-иное");
    }

    /// 4.7L-2-diag — видео-контейнеры: EBML (mkv/webm) и ISO-BMFF
    /// (mp4/mov/cr3/avif/heic — бренд в байтах 8..12).
    #[test]
    fn sniff_video_containers() {
        assert_eq!(
            super::sniff_bytes(&[0x1A, 0x45, 0xDF, 0xA3, 0x9F]),
            "видео (mkv/webm)"
        );
        let mp4 = b"\x00\x00\x00\x20ftypisom";
        assert_eq!(super::sniff_bytes(mp4), "видео (mp4)");
        let mov = b"\x00\x00\x00\x20ftypqt  ";
        assert_eq!(super::sniff_bytes(mov), "видео (mov)");
        let cr3 = b"\x00\x00\x00\x20ftypcrx ";
        assert_eq!(super::sniff_bytes(cr3), "raw (cr3)");
        let avif = b"\x00\x00\x00\x20ftypavif";
        assert_eq!(super::sniff_bytes(avif), "avif");
        let heic = b"\x00\x00\x00\x20ftypheic";
        assert_eq!(super::sniff_bytes(heic), "heic");
    }

    /// 4.7L-2-diag — TIFF-магия II*/MM0* — это и обычный tiff, и
    /// большинство RAW (CR2/NEF/ARW/DNG/…); RAF и ICO/PSD/JXL — свои.
    #[test]
    fn sniff_tiff_raw_and_misc() {
        assert_eq!(super::sniff_bytes(&[0x49, 0x49, 0x2A, 0x00]), "tiff/raw");
        assert_eq!(super::sniff_bytes(&[0x4D, 0x4D, 0x00, 0x2A]), "tiff/raw");
        assert_eq!(super::sniff_bytes(b"FUJIFILMCCD-RAW \x02\x00"), "raw (raf)");
        assert_eq!(super::sniff_bytes(&[0x00, 0x00, 0x01, 0x00]), "ico");
        assert_eq!(super::sniff_bytes(b"8BPS\x00\x01"), "psd");
        assert_eq!(
            super::sniff_bytes(&[0x00, 0x00, 0x00, 0x0C, b'J', b'X', b'L', b' ']),
            "jxl"
        );
        assert_eq!(super::sniff_bytes(&[0xFF, 0x0A]), "jxl");
    }

    /// 4.7L-2-diag — мусор и пустой буфер = «неизвестно» (реально битые);
    /// файл, который не открывается, — отдельная корзина «не читается».
    #[test]
    fn sniff_unknown_and_unreadable() {
        assert_eq!(super::sniff_bytes(&[0x00, 0x01, 0x02, 0x03]), "неизвестно");
        assert_eq!(super::sniff_bytes(&[]), "неизвестно");
        assert_eq!(
            super::sniff_format_label(std::path::Path::new("Z:/definitely/not/existing/file.bin")),
            "не читается"
        );
    }
}
