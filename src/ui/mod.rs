//! Слой интерфейса на Slint.
//!
//! Экран блокировки (одно окно): точки PIN + встроенная клавиатура + автовход.
//! Биометрия запускается в ОТДЕЛЬНОМ потоке (op.get() блокирующий) и результат
//! возвращается в UI-поток через slint::invoke_from_event_loop. Флаг «идёт» защищает
//! от повторных запросов (иначе дедлок/зависание).

slint::include_modules!();

mod anim_cache;
mod anim_playback;

use anim_cache::{AnimFrame, AnimFramesCache, ANIM_CACHE_MAX_BYTES};
use anim_playback::{effective_delay_ms, AnimPlayback};

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::core::ports::{AuthOutcome, Authenticator};
use crate::security::SecurityService;

/// 4.3e — следующий индекс при авто-скипе битого файла.
/// dir = +1 (вперёд) или −1 (назад). Без зацикливания: клампим по границам,
/// возвращаем −1, если вышли за край (сигнал «годных больше нет»).
/// Чистая функция на уровне модуля — легко тестировать без UI.
pub fn next_valid_index(next_index: i32, dir: i32, count: i32) -> i32 {
    if count <= 0 {
        return -1;
    }
    let ni = next_index + dir;
    if ni < 0 || ni >= count {
        -1 // край списка — дальше годных нет
    } else {
        ni
    }
}

/// 4.4a — режим листания в просмотрщике (per-project, хранится в сессии).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavMode {
    /// До края: на первом/последнем фото стоп (не заворачиваем).
    Edge,
    /// Циклично: на границе заворачиваем на противоположный край.
    Cycle,
    /// Случайный порядок: next/prev выбирают случайный кадр из всего списка.
    Random,
}

impl NavMode {
    /// Обратно в строку для сохранения в сессию.
    pub fn as_str(&self) -> &'static str {
        match self {
            NavMode::Edge => "edge",
            NavMode::Cycle => "cycle",
            NavMode::Random => "random",
        }
    }
}

/// Разбор режима из строки сессии: "edge" | "cycle" | "random".
/// Пустая строка (старые JSON без поля) и неизвестные значения → Edge
/// (по умолчанию — самое предсказуемое, чинит «прыжок в конец»).
/// Реализуем стандартный трейт FromStr (идиоматично, как советует clippy).
impl std::str::FromStr for NavMode {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "cycle" => NavMode::Cycle,
            "random" => NavMode::Random,
            _ => NavMode::Edge,
        })
    }
}

/// 4.4a — следующий индекс листания по режиму (edge/cycle).
/// cur — текущий индекс, dir — +1 (next) / −1 (prev), count — размер списка.
/// Возвращает Some(новый индекс) или None (стоп: в edge-режиме дошли до края).
/// Чистая функция — легко тестировать без UI.
///   • edge:  cur+dir; за границей [0, count) → None (стоим на месте).
///   • cycle: cur+dir с заворачиванием (0−1 → count−1; count−1+1 → 0).
/// 4.4c-3 — broken: индексы битых файлов; навигация их ПРОПУСКАЕТ (шагает
/// дальше, не грузя заведомо битое). Битые исключены из листания во всех
/// режимах — иначе random постоянно попадал в битые серии (миниатюра битого
/// мелькала, скип уводил на другой кадр).
/// Random-режим обрабатывает nav_random_next (шаг по перемешанному списку).
pub fn nav_next(
    cur: i32,
    count: i32,
    dir: i32,
    mode: NavMode,
    broken: &std::collections::HashSet<i32>,
) -> Option<i32> {
    if count <= 0 {
        return None; // пустой список — листать нечего
    }
    match mode {
        NavMode::Edge => {
            let mut ni = cur + dir;
            loop {
                if ni < 0 || ni >= count {
                    return None; // край — стоп, не переходим
                }
                if !broken.contains(&ni) {
                    return Some(ni); // годное — сюда
                }
                ni += dir; // битое — шагаем дальше в том же направлении
            }
        }
        NavMode::Cycle => {
            // Заворачиваем и шагаем, пока не найдём годное (или не обойдём всё).
            let mut ni = cur + dir;
            for _ in 0..count {
                if ni >= count {
                    ni = 0;
                } else if ni < 0 {
                    ni = count - 1;
                }
                if !broken.contains(&ni) {
                    return Some(ni);
                }
                ni += dir;
            }
            None // весь список битый — стоп
        }
        // 4.4c — random идёт по перемешанному списку (см. nav_random_next);
        // сюда не попадаем, но вернуть что-то безопасное нужно для полноты.
        NavMode::Random => {
            let ni = cur + dir;
            if ni < 0 || ni >= count {
                None
            } else {
                Some(ni)
            }
        }
    }
}

/// 4.4c — перемешанный порядок индексов [0..count): биекция, каждый индекс
/// ровно один раз (идея заказчика: «мешаем номера и листаем по списку»).
/// Порядок фиксируется ОДИН раз при включении random — поэтому «назад»
/// честно возвращает предыдущее просмотренное фото, а не новый случайный.
pub fn shuffled_order(count: i32) -> Vec<i32> {
    use rand::seq::SliceRandom;
    let mut v: Vec<i32> = (0..count.max(0)).collect();
    v.shuffle(&mut rand::thread_rng());
    v
}

/// 4.4c — шаг по перемешанному списку (random-режим).
/// order — перестановка индексов фото; pos — текущая ПОЗИЦИЯ в списке;
/// dir — +1 (next) / −1 (prev) по СПИСКУ. Возвращает Some((новая позиция,
/// индекс фото)) или None (дошли до края списка — стоп, как в edge).
/// 4.4c-3 — broken: битые позиции списка ПРОПУСКАЕМ (шагаем дальше, пока
/// не найдём годное) — random больше не попадает в битые серии.
/// Чистая функция — легко тестировать.
pub fn nav_random_next(
    order: &[i32],
    pos: i32,
    dir: i32,
    broken: &std::collections::HashSet<i32>,
) -> Option<(i32, i32)> {
    let len = order.len() as i32;
    if len <= 0 {
        return None; // пустой список — листать нечего
    }
    let mut np = pos + dir;
    while np >= 0 && np < len {
        let photo = order[np as usize];
        if !broken.contains(&photo) {
            return Some((np, photo)); // годное — сюда
        }
        np += dir; // битое — следующая позиция списка
    }
    None // край перемешанного списка — стоп
}

/// 4.4c-4 — соседи для ПРЕДЗАГРУЗКИ по текущему порядку листания.
/// Возвращает до `radius` индексов фото в каждую сторону:
///   • обычный порядок (edge/cycle): index±1..radius;
///   • random: фото с позиций shuffle_pos±1..radius ПЕРЕМЕШАННОГО списка
///     (раньше грузили линейных index±1 — в random это чужие кадры, и их
///     результат перебивал текущий: «чёткое само меняется на иное»).
/// broken-индексы пропускаем (заведомо битые — не тратим время воркера).
/// Чистая функция — легко тестировать.
pub fn preload_neighbors(
    index: i32,
    count: i32,
    radius: i32,
    order: Option<&[i32]>,
    shuffle_pos: i32,
    broken: &std::collections::HashSet<i32>,
) -> Vec<i32> {
    let mut out = Vec::new();
    if count <= 0 {
        return out;
    }
    for step in 1..=radius.max(0) {
        for dir in [1, -1] {
            let photo = match order {
                // random: сосед = фото с соседней ПОЗИЦИИ перемешанного списка.
                Some(ord) => {
                    let np = shuffle_pos + dir * step;
                    match ord.get(np.max(0) as usize) {
                        Some(&p) if np >= 0 && np < ord.len() as i32 => p,
                        _ => continue, // край списка — нет соседа
                    }
                }
                // обычный порядок: линейный сосед.
                None => {
                    let ni = index + dir * step;
                    if ni < 0 || ni >= count {
                        continue; // край — нет соседа
                    }
                    ni
                }
            };
            if photo < 0 || photo >= count || broken.contains(&photo) {
                continue; // битое или вне списка — не грузим
            }
            out.push(photo);
        }
    }
    out
}

/// 4.3g — кламп смещения фото «край в край» ПО КАРТИНКЕ (contain-fit).
/// Элемент Image имеет размер vp*zoom, но картинка вписана (contain) и у
/// узких/широких фото МЕНЬШЕ элемента. Кламп по элементу позволял утащить
/// фото целиком за экран (чёрный экран). Считаем реальный размер картинки:
/// scale = min(vp_w/iw, vp_h/ih) — во сколько раз вписана картинка при zoom=1;
/// размер картинки на экране = iw * scale * zoom. Если картинка по оси меньше
/// viewport — двигать нельзя (max=0), иначе max = (size − vp)/2.
/// Чистая функция — легко тестировать.
pub fn clamp_offset(
    offset: f32,
    zoom: f32,
    viewport: f32,
    img: f32,
    other_img: f32,
    other_vp: f32,
) -> f32 {
    // Размер картинки по этой оси при вписывании (contain) и текущем zoom.
    let contain_scale = if img > 0.0 && other_img > 0.0 {
        (viewport / img).min(other_vp / other_img)
    } else {
        1.0
    };
    let size = img * contain_scale * zoom;
    let max = ((size - viewport) / 2.0).max(0.0);
    offset.clamp(-max, max)
}

/// 4.7a-fix4 — задержка кадра в МИЛЛИСЕКУНДАХ из дроби `image::Delay`.
///
/// Контракт единиц: `numer_denom_ms()` крейта image отдаёт числитель и
/// знаменатель дроби, значение которой УЖЕ миллисекунды (кадр GIF «три
/// сотых секунды» → (30, 1) = 30 мс; см. `impl From<Delay> for Duration`
/// в крейте — ratio идёт прямо в `from_millis`). Ошибка 4.7a: умножение
/// на 1000 трактовало дробь как СЕКУНДЫ → кадр 30 мс спал 30 СЕКУНД,
/// «GIF замер на первом кадре». Защиты: нулевой знаменатель → 100 мс
/// (кадр без задержки), минимум 20 мс (не мгновенный проскок); длинные
/// паузы НЕ режем — это легитимные кадры-паузы GIF.
pub fn anim_delay_ms((numer, denom): (u32, u32)) -> u64 {
    if denom == 0 {
        return 100;
    }
    (numer as u64 / denom as u64).max(20)
}

/// Выводит окно приложения на передний план и даёт ему фокус (только Windows).
///
/// Системное окно Windows Hello всплывает активным поверх окна, которое вызвало запрос.
/// Поэтому перед биометрией поднимаем наше окно, чтобы Hello сразу получал фокус и палец
/// можно было приложить без клика мышкой.
#[cfg(windows)]
fn bring_window_to_foreground(ui: &LockScreen) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::Input::KeyboardAndMouse::{SetActiveWindow, SetFocus};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
        ShowWindow, SW_SHOW,
    };

    let handle = ui.window().window_handle();
    let raw = match handle.window_handle() {
        Ok(r) => r,
        Err(_) => return,
    };
    if let RawWindowHandle::Win32(win32) = raw.as_raw() {
        let hwnd = HWND(win32.hwnd.get() as *mut _);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);

            // Трюк AttachThreadInput: временно «подключаем» ввод нашего потока
            // к потоку текущего переднего окна, чтобы SetForegroundWindow прошёл.
            let our_thread = GetCurrentThreadId();
            let fg = GetForegroundWindow();
            let mut fg_pid = 0u32;
            let fg_thread = if fg.0.is_null() {
                0
            } else {
                GetWindowThreadProcessId(fg, Some(&mut fg_pid as *mut u32))
            };

            let attached = if fg_thread != 0 && fg_thread != our_thread {
                AttachThreadInput(our_thread, fg_thread, true).as_bool()
            } else {
                false
            };

            let _ = BringWindowToTop(hwnd);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetActiveWindow(hwnd);
            let _ = SetFocus(Some(hwnd));

            if attached {
                let _ = AttachThreadInput(our_thread, fg_thread, false);
            }
        }
    }
}

#[cfg(not(windows))]
fn bring_window_to_foreground(_ui: &LockScreen) {}

/// Результат попытки входа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockResult {
    Granted,
    Denied,
}

/// Контроллер экрана блокировки: связывает UI с сервисом безопасности.
pub struct LockController {
    service: Arc<SecurityService>,
    granted: Mutex<bool>,
}

impl LockController {
    pub fn new(pin_file: PathBuf) -> Self {
        Self {
            service: Arc::new(SecurityService::new(pin_file)),
            granted: Mutex::new(false),
        }
    }

    pub fn has_pin(&self) -> bool {
        self.service.has_pin()
    }

    /// Проверка PIN (при входе). Синхронная, но быстрая — можно в UI-потоке.
    pub fn verify_pin(&self, pin: &str) -> LockResult {
        match self.service.verify_pin(pin) {
            Ok(AuthOutcome::Granted) => {
                *self.granted.lock().unwrap() = true;
                LockResult::Granted
            }
            _ => LockResult::Denied,
        }
    }

    /// Создание PIN (при первом запуске). Валидация внутри сервиса.
    pub fn create_pin(&self, pin: &str) -> Result<(), String> {
        self.service.set_pin(pin).map_err(|e| e.to_string())
    }

    /// Биометрия (Windows Hello). БЛОКИРУЮЩАЯ — вызывать только в отдельном потоке.
    pub fn biometric(&self) -> LockResult {
        match self.service.authenticate_biometric() {
            Ok(AuthOutcome::Granted) => {
                *self.granted.lock().unwrap() = true;
                LockResult::Granted
            }
            _ => LockResult::Denied,
        }
    }

    pub fn is_granted(&self) -> bool {
        *self.granted.lock().unwrap()
    }
}

/// Запускает экран блокировки и блокирует поток до успешного входа.
pub fn run_lock_screen(controller: Arc<LockController>) -> Result<(), slint::PlatformError> {
    let ui = LockScreen::new()?;

    let creating = !controller.has_pin();
    ui.set_has_pin(!creating);
    ui.set_status_text(if creating {
        "Photo Explorer — создайте PIN".into()
    } else {
        "Photo Explorer".into()
    });
    #[cfg(windows)]
    ui.set_biometric_available(true);
    #[cfg(not(windows))]
    ui.set_biometric_available(false);

    // Состояние ввода PIN (живёт в UI-потоке, Rc безопасен).
    let entered = Rc::new(RefCell::new(String::new()));
    let max_len = 4usize;

    // Флаг «биометрия идёт» — защита от повторных запросов (дедлок).
    let biometric_in_progress = Arc::new(AtomicBool::new(false));

    /// Запускает биометрию в фоновом потоке; результат обрабатывается в UI-потоке.
    /// Если проверка уже идёт — игнорирует вызов.
    fn request_biometric(
        controller: &Arc<LockController>,
        ui_weak: slint::Weak<LockScreen>,
        flag: &Arc<AtomicBool>,
    ) {
        // Защита от повторного запуска: если уже идёт — выходим.
        if flag
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        // Поднимаем наше окно на передний план, чтобы системное окно Windows Hello
        // всплыло активным (палец можно приложить без клика мышкой).
        if let Some(w) = ui_weak.upgrade() {
            bring_window_to_foreground(&w);
        }
        let controller = controller.clone();
        let flag = flag.clone();
        let ui_weak = ui_weak.clone();
        std::thread::spawn(move || {
            let result = controller.biometric();
            // Возвращаем результат в UI-поток.
            let ui_weak = ui_weak.clone();
            let flag = flag.clone();
            let _ = slint::invoke_from_event_loop(move || {
                flag.store(false, Ordering::SeqCst);
                if let Some(w) = ui_weak.upgrade() {
                    match result {
                        LockResult::Granted => {
                            let _ = w.hide();
                        }
                        LockResult::Denied => {
                            w.set_error_text("Биометрия отклонена или недоступна".into());
                        }
                    }
                }
            });
        });
    }

    // Колбэк «Войти по PIN» — показать встроенную клавиатуру.
    {
        let ui_weak = ui.as_weak();
        ui.on_enter_pin(move || {
            if let Some(w) = ui_weak.upgrade() {
                w.set_show_keyboard(true);
                w.set_error_text("".into());
            }
        });
    }

    // Счётчик неверных попыток PIN. После 3 — приложение закрывается.
    let failed_attempts = Arc::new(AtomicU32::new(0));
    const MAX_ATTEMPTS: u32 = 3;

    // Колбэк нажатия цифры — накопление + автовход на 4-й цифре.
    {
        let ctrl = controller.clone();
        let ui_weak = ui.as_weak();
        let entered = entered.clone();
        let failed_attempts = failed_attempts.clone();
        ui.on_digit_pressed(move |digit| {
            let mut buf = entered.borrow_mut();
            if buf.len() < max_len {
                buf.push(char::from_digit(digit as u32, 10).unwrap_or('0'));
            }
            let count = buf.len() as i32;
            if let Some(w) = ui_weak.upgrade() {
                w.set_entered_count(count);
                w.set_error_text("".into());
            }
            // На 4-й цифре — проверяем (автовход).
            if buf.len() == max_len {
                let pin = buf.clone();
                let creating = !ctrl.has_pin();
                if creating {
                    match ctrl.create_pin(&pin) {
                        Ok(()) => {
                            if let Some(w) = ui_weak.upgrade() {
                                w.set_status_text("PIN создан. Войдите по PIN.".into());
                                w.set_error_text("".into());
                                w.set_entered_count(0);
                            }
                            *buf = String::new();
                        }
                        Err(e) => {
                            if let Some(w) = ui_weak.upgrade() {
                                w.set_error_text(e.into());
                                w.set_entered_count(0);
                            }
                            *buf = String::new();
                        }
                    }
                } else {
                    match ctrl.verify_pin(&pin) {
                        LockResult::Granted => {
                            // Успех — сбрасываем счётчик неверных попыток.
                            failed_attempts.store(0, Ordering::SeqCst);
                            if let Some(w) = ui_weak.upgrade() {
                                let _ = w.hide();
                            }
                        }
                        LockResult::Denied => {
                            let attempts = failed_attempts.fetch_add(1, Ordering::SeqCst) + 1;
                            let left = MAX_ATTEMPTS.saturating_sub(attempts);
                            if left == 0 {
                                // Лимит исчерпан — закрываем приложение.
                                if let Some(w) = ui_weak.upgrade() {
                                    w.set_error_text("Слишком много попыток. Закрытие.".into());
                                }
                                slint::quit_event_loop().ok();
                            } else if let Some(w) = ui_weak.upgrade() {
                                w.set_error_text(
                                    format!("Неверный PIN. Осталось попыток: {left}").into(),
                                );
                                w.set_entered_count(0);
                            }
                            *buf = String::new();
                        }
                    }
                }
            }
        });
    }

    // Колбэк стирания.
    {
        let ui_weak = ui.as_weak();
        let entered = entered.clone();
        ui.on_backspace(move || {
            let mut buf = entered.borrow_mut();
            buf.pop();
            if let Some(w) = ui_weak.upgrade() {
                w.set_entered_count(buf.len() as i32);
                w.set_error_text("".into());
            }
        });
    }

    // Колбэк «Назад» — спрятать клавиатуру, вернуться на главный экран блокировки.
    {
        let ui_weak = ui.as_weak();
        let entered = entered.clone();
        ui.on_back_to_main(move || {
            let mut buf = entered.borrow_mut();
            buf.clear();
            if let Some(w) = ui_weak.upgrade() {
                w.set_show_keyboard(false);
                w.set_entered_count(0);
                w.set_error_text("".into());
            }
        });
    }

    // Колбэк «Отпечаток пальца» — ручной повтор (уважает флаг «идёт»).
    {
        let ctrl = controller.clone();
        let ui_weak = ui.as_weak();
        let flag = biometric_in_progress.clone();
        ui.on_biometric(move || {
            request_biometric(&ctrl, ui_weak.clone(), &flag);
        });
    }

    // Автозапрос биометрии при старте УБРАН намеренно: SetForegroundWindow из фона
    // блокируется Windows (защита от кражи фокуса), и системный диалог Hello всплывал бы в фоне.
    // Биометрия запускается только по клику пользователя по кнопке «Отпечаток пальца» —
    // тогда наше окно уже активно (input-driven foreground), и Hello всплывает поверх активным.
    // UX: «Запустил → Кликнул Отпечаток → Приложил → Зашёл».

    ui.run()
}

/// Запускает главный экран галереи.
pub fn run_gallery() -> Result<(), slint::PlatformError> {
    use crate::core::models::{Photo, ScanMode, Session, SessionSummary};
    use crate::core::ports::{PhotoScanner, SessionStore, ThumbnailProvider};
    use crate::scanner::FileSystemScanner;
    use crate::session::{fingerprint_of, JsonSessionStore};
    use crate::thumbnails::FileSystemThumbnailProvider;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 4.7 — подпись даты открытия сессии для человека («2026-10-29 10:30»).
    fn opened_label(secs: u64) -> String {
        let (y, mo, d, h, mi, _s) = crate::logging::epoch_to_local(secs as i64);
        format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}")
    }

    /// 4.7g — обновляет список сессий на стартовом экране из хранилища.
    /// Карточки получают обложку (миниатюра первого фото из дискового кэша —
    /// мгновенно, без декодирования оригиналов) и счётчик фото.
    fn refresh_session_list(
        store: &JsonSessionStore,
        thumbs: &Arc<FileSystemThumbnailProvider>,
        ui_weak: &slint::Weak<GalleryScreen>,
    ) {
        let summaries: Vec<SessionSummary> = match store.summaries() {
            Ok(list) => list,
            Err(e) => {
                log::error!("sessions: не удалось прочитать index.json: {e}");
                return;
            }
        };
        if let Some(w) = ui_weak.upgrade() {
            let items: Vec<SessionItem> = summaries
                .into_iter()
                .map(|s| {
                    // Обложка: миниатюра первого фото (кэш на диске — быстро).
                    let cover = s.cover_path.as_ref().map(|p| {
                        let photo = Photo {
                            path: p.clone(),
                            file_name: p
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default(),
                            size_bytes: 0,
                            modified: 0,
                        };
                        match thumbs.thumbnail(&photo) {
                            Ok(t) => {
                                let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(
                                    t.width, t.height,
                                );
                                let pixels = buf.make_mut_slice();
                                for (dst, chunk) in pixels.iter_mut().zip(t.data.as_chunks::<4>().0)
                                {
                                    dst.r = chunk[0];
                                    dst.g = chunk[1];
                                    dst.b = chunk[2];
                                    dst.a = chunk[3];
                                }
                                slint::Image::from_rgba8(buf)
                            }
                            Err(_) => slint::Image::default(),
                        }
                    });
                    SessionItem {
                        id: s.id.into(),
                        name: s.name.into(),
                        folder: s.folder.to_string_lossy().to_string().into(),
                        opened_label: opened_label(s.last_opened).into(),
                        is_project: s.is_project,
                        photo_count: s.photo_count as i32,
                        cover: cover.unwrap_or_default(),
                    }
                })
                .collect();
            w.set_session_list(slint::ModelRc::new(slint::VecModel::from(items)));
        }
    }

    /// LRU-кэш полных изображений в памяти: путь -> (RGBA-данные, ширина, высота).
    type ViewerCache = lru::LruCache<std::path::PathBuf, (Vec<u8>, u32, u32)>;

    /// Безопасно берёт блокировку даже у отравленного (poisoned) мьютекса.
    /// Если фоновый поток паниковал, мьютекс становится отравленным и обычный
    /// `.unwrap()` каскадно ронял бы весь UI. Здесь мы сознательно достаём
    /// данные из отравленной блокировки — лучше показать устаревшее состояние,
    /// чем уронить приложение.
    fn lock_or_recover<T>(lock: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    let ui = GalleryScreen::new()?;
    // Окно остаётся обычным оконным приложением. На весь экран разворачивается
    // только само фото в режиме просмотра — через overlay-слой в lock.slint
    // (Rectangle с фоном #000000ee на весь parent). Разворачивать всё окно
    // не нужно: заказчик хочет обычное окно + полноэкранный просмотр фото.

    // Сканер и поставщик миниатюр. Кэш миниатюр — в temp.
    let cache_dir = std::env::temp_dir().join("photo_explorer_thumbs_cache");
    let scanner = Arc::new(FileSystemScanner::new());
    let thumbs = Arc::new(FileSystemThumbnailProvider::new(cache_dir, 160));

    // Список Photo (с путями к оригиналам) — нужен для viewer. Arc<Mutex> — доступ из потоков.
    let photos: Arc<Mutex<Vec<Photo>>> = Arc::new(Mutex::new(Vec::new()));

    // 4.7 — сессионное хранилище: %LOCALAPPDATA%/photo_explorer/sessions.
    // Один раз отсканировали папку — при повторном входе открываем мгновенно.
    let sessions_dir = dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("photo_explorer")
        .join("sessions");
    let store = Arc::new(JsonSessionStore::new(sessions_dir));
    // Показываем экран «Проекты» при входе (если есть сохранённые сессии).
    {
        let ui_weak = ui.as_weak();
        let has_any = store.summaries().map(|l| !l.is_empty()).unwrap_or(false);
        if has_any {
            if let Some(w) = ui_weak.upgrade() {
                w.set_show_start(true);
            }
        }
        refresh_session_list(&store, &thumbs, &ui_weak);
    }

    // LRU-кэш полных изображений (сырой RGBA, Send) — для воркера.
    // 4.4c-4 — 40 (было 20): предзагрузка 3×2 соседей + текущие кадры
    // должны помещаться, иначе соседи вытесняют друг друга до показа.
    let viewer_cache: Arc<Mutex<ViewerCache>> =
        Arc::new(Mutex::new(ViewerCache::new(40.try_into().unwrap())));

    // 4.3d-v7 — UI-кэш ГОТОВЫХ slint::Image (ключ — путь).
    // КОРЕНЬ фикса затупа: раньше на КАЖДЫЙ кадр (даже из кэша) UI-поток строил
    // slint::Image из сырого RGBA попиксельно (до 26 МБ на фото) — при 20-30
    // событиях/сек это гигабайты копирования в главном потоке → затуп.
    // Теперь: воркер один раз строит slint::Image и кладёт сюда; показ из кэша
    // = set_viewer_image(img.clone()) — clone дешёвый (внутри Rc). Без копирования.
    // slint::Image НЕ Send → кэш в Rc<RefCell>, живёт только в UI-потоке.
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;
    type ImageCache = Rc<RefCell<HashMap<std::path::PathBuf, slint::Image>>>;
    let image_cache: ImageCache = Rc::new(RefCell::new(HashMap::new()));
    const IMAGE_CACHE_MAX: usize = 40; // 4.4c-4 — под предзагрузку 3 соседей

    // Поколение загрузки: инкрементируется при каждом переключении фото.
    // Фоновый поток сверяет своё поколение перед применением результата —
    // если пользователь уже листает дальше, устаревший результат отбрасывается.
    // Это убирает «мигание» чужими фото и лавину потоков при быстром листании.
    let viewer_generation: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));

    // 4.4b — поколение ГАЛЕРЕИ: инкремент при открытии проекта/скане папки.
    // Фоновые батчи миниатюр сверяют его перед добавлением в модель — если
    // пользователь открыл другой проект, батчи старого отменяются (не будут
    // подмешиваться в чужую сетку).
    let gallery_gen: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));

    // 4.3e — направление последнего листания (+1 вперёд / −1 назад).
    // Нужно для авто-скипа битого файла: скип идём в ту же сторону, куда листал
    // пользователь. AtomicI32 — доступ из воркера и UI без блокировок.
    use std::sync::atomic::AtomicI32;
    let last_direction: Arc<AtomicI32> = Arc::new(AtomicI32::new(1));

    // 4.6c (Фаза 2) — глобальные настройки «Ультра-скорость»: галка (ВЫКЛ
    // по умолчанию — бережёт батарею) + число ядер (по умолчанию половина).
    // Воркер читает атомики при каждом декоде — смена настройки применяется
    // к СЛЕДУЮЩЕМУ кадру без перезапуска. Хранение: %LOCALAPPDATA%.
    let global_settings: Arc<crate::settings::GlobalSettings> = Arc::new(crate::settings::load());
    let (ultra_on, ultra_threads) = global_settings.snapshot();
    log::info!("viewer: настройки Ультра-скорости: включена={ultra_on}, ядер={ultra_threads}");

    // Максимальная сторона изображения для viewer (downscale до размера экрана).
    const VIEWER_MAX_SIZE: u32 = 2560;

    // 4.3e — автоскрытие управления viewer: кнопки гаснут через 2.5с бездействия.
    const CONTROLS_HIDE_MS: u64 = 2500;
    let controls_timer = std::rc::Rc::new(slint::Timer::default());

    // 4.3d-v6 — ОДИН постоянный воркер-поток (вместо spawn на задачу).
    // Раньше на каждое нажатие создавался новый ОС-поток; при удержании их
    // сотни → ОС захлёбывался, память RGBA не успевала чиститься → затуп.
    // Теперь: один воркер живёт всё время, UI шлёт задачи в канал (producer-
    // consumer). Воркер дренирует очередь (берёт последнюю актуальную по gen),
    // декодирует ОДНО фото и отдаёт в UI. Всегда ровно 1 фоновый поток.
    use std::sync::mpsc;
    // 4.6a [CONC] — идентификатор задачи в логах = generation (уникален
    // на каждый переход); отдельный счётчик не нужен (KISS).
    // 4.6b (Фаза 1) — ДВА канала на тот же ОДИН поток. Логи [CONC] показали
    // корень тормоза «размытое висит ~10 сек»: тяжёлый сосед-Preload
    // (downscale 3840x2160 — 5.5 с) вставал в общей FIFO-очереди ПЕРЕД
    // текущим кадром. Теперь priority несёт ТОЛЬКО Photo (текущий кадр),
    // bg — только Preload (соседи); воркер опустошает priority первым и
    // лишь при пустом берёт ОДНУ bg-задачу (см. recv_priority_aware).
    enum LoadTask {
        Photo {
            index: i32,
            path: std::path::PathBuf,
            generation: u64,
            // 4.3e — глубина авто-скипа битых файлов (0 = обычный показ).
            // Защита от зацикливания: если превысила размер списка — стоп + плашка.
            skip_depth: u32,
        },
        Preload {
            path: std::path::PathBuf,
        },
    }
    let (priority_tx, priority_rx) = mpsc::channel::<LoadTask>();
    let (bg_tx, bg_rx) = mpsc::channel::<LoadTask>();
    // ui_weak для воркера (Send): Weak<GalleryScreen> — Send.
    let ui_weak_for_viewer = ui.as_weak();
    // Воркер-поток: цикл приёма задач. Send-данные (Vec<u8>) отдаём в UI
    // через invoke_from_event_loop (slint::Image — не Send, строим в UI-потоке).
    {
        let viewer_cache = viewer_cache.clone();
        let viewer_generation = viewer_generation.clone();
        let last_direction = last_direction.clone();
        let global_settings = global_settings.clone();
        let ui_weak_for_viewer = ui_weak_for_viewer.clone();
        std::thread::spawn(move || {
            log::info!("viewer worker: запущен (1 поток, каналы priority/bg)");
            let viewer_cache = viewer_cache;
            let viewer_generation = viewer_generation;
            let last_direction = last_direction;
            let global_settings = global_settings;
            // 4.6b — пауза, когда ОБА канала пусты: не жжём CPU холостым
            // опросом; фоновая задача при простое ждёт максимум это время.
            let idle_poll = std::time::Duration::from_millis(100);
            while let Some(task) = recv_priority_aware(&priority_rx, &bg_rx, idle_poll) {
                match task {
                    LoadTask::Photo {
                        index,
                        path,
                        generation: my_gen,
                        skip_depth,
                    } => {
                        // Дренируем очередь: если в канале ещё есть задачи —
                        // берём последнюю (промежуточные пропускаем, не декодируя).
                        // 4.6a [CONC] — drain_to_last возвращает также СКОЛЬКО
                        // задач было пропущено (глубина очереди на момент приёма).
                        let (index, path, my_gen, skip_depth, skipped) =
                            drain_to_last(&priority_rx, index, path, my_gen, skip_depth);
                        log::debug!(
                            "[CONC] worker: Photo idx={index} gen={my_gen} depth={skip_depth} skipped={skipped}"
                        );
                        // Устаревшая задача — пропускаем (пользователь ушёл дальше).
                        if viewer_generation.load(Ordering::SeqCst) != my_gen {
                            log::debug!("[CONC] worker: устарела gen={my_gen}, пропускаю");
                            continue;
                        }
                        let decode_start = std::time::Instant::now();
                        let result = load_full_image(path, viewer_cache.clone(), &global_settings);
                        log::debug!(
                            "[CONC] worker: Photo idx={index} декод за {} мс ({})",
                            decode_start.elapsed().as_millis(),
                            resize_mode_label(&global_settings)
                        );
                        let gen = viewer_generation.clone();
                        let dir = last_direction.load(Ordering::SeqCst);
                        let ui_weak = ui_weak_for_viewer.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            // Устарел к моменту готовности — не показываем.
                            let cur = gen.load(Ordering::SeqCst);
                            if cur != my_gen {
                                log::debug!("worker: результат устарел my={my_gen} cur={cur}");
                                return;
                            }
                            if let Some(w) = ui_weak.upgrade() {
                                // 4.4c-4 — двойная защита: результат применяется
                                // к экрану ТОЛЬКО если текущий viewer-index всё
                                // ещё равен индексу задачи (поколение уже
                                // сверено выше). Устраняет «чёткое само
                                // меняется на иное» при гонках предзагрузки.
                                if w.get_viewer_index() != index {
                                    log::debug!(
                                        "worker: индекс ушёл (задача={index}, текущий={}) — не показываем",
                                        w.get_viewer_index()
                                    );
                                    return;
                                }
                                match result {
                                    Ok((data, w_px, h_px)) => {
                                        let mut buf =
                                            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(
                                                w_px, h_px,
                                            );
                                        let pixels = buf.make_mut_slice();
                                        for (dst, chunk) in
                                            pixels.iter_mut().zip(data.as_chunks::<4>().0)
                                        {
                                            dst.r = chunk[0];
                                            dst.g = chunk[1];
                                            dst.b = chunk[2];
                                            dst.a = chunk[3];
                                        }
                                        // 4.3g — размеры картинки для клампа «край в край».
                                        w.set_viewer_img_w(w_px as f32);
                                        w.set_viewer_img_h(h_px as f32);
                                        w.set_viewer_image(slint::Image::from_rgba8(buf));
                                        // 4.3e — успешный показ: гасим плашку ошибки.
                                        w.set_viewer_error("".into());
                                        log::debug!("[CONC] put: worker idx={index} gen={my_gen}");
                                    }
                                    Err(e) => {
                                        // 4.3e — авто-скип битого файла: просим UI
                                        // показать следующий кадр в текущем направлении.
                                        // UI-колбэк request-skip сам решает границы и
                                        // защиту от зацикливания (ему доступен Rc-кэш).
                                        // 4.3h — передаём my_gen: если пользователь уже
                                        // ушёл дальше, скип будет проигнорирован.
                                        log::error!("worker: битый файл index={index}: {e}");
                                        let next = index + dir;
                                        let next_depth = skip_depth.saturating_add(1);
                                        w.invoke_request_skip(
                                            next,
                                            dir,
                                            next_depth as i32,
                                            my_gen as i32,
                                        );
                                    }
                                }
                            }
                        });
                    }
                    LoadTask::Preload { path } => {
                        // Предзагрузка соседа: если уже в кэше — пропускаем.
                        if lock_or_recover(&viewer_cache).contains(&path) {
                            continue;
                        }
                        // 4.6a [CONC] — фон: тайминг декода соседа (мешал ли
                        // он текущему кадру — видно в логе). Путь логируем ДО
                        // перемещения в load_full_image.
                        let shown = path.display().to_string();
                        let t0 = std::time::Instant::now();
                        let _ = load_full_image(path, viewer_cache.clone(), &global_settings);
                        log::debug!(
                            "[CONC] worker: Preload «{shown}» за {} мс ({})",
                            t0.elapsed().as_millis(),
                            resize_mode_label(&global_settings)
                        );
                    }
                }
            }
            log::info!("viewer worker: завершён");
        });
    }
    // 4.6b (Фаза 1) — приём задачи с ПРИОРИТЕТОМ текущего кадра:
    // 1) пока в priority есть задачи — читаем только его (Photo всегда
    //    впереди соседей: тяжёлый Preload больше не встаёт в очередь перед
    //    кадром, который пользователь смотрит прямо сейчас);
    // 2) priority пуст — ОДНА задача из bg (между соседями снова проверяем
    //    priority — нажатие пользователя всегда побеждает);
    // 3) оба пусты — блокируемся на priority с таймаутом idle_poll
    //    (холостого опроса нет, CPU не жжём).
    // None = priority-канал закрыт (UI ушёл) — воркер завершается. bg-канал
    // живёт и закрывается вместе с priority (оба tx лежат в ViewerCtx).
    fn recv_priority_aware(
        priority_rx: &mpsc::Receiver<LoadTask>,
        bg_rx: &mpsc::Receiver<LoadTask>,
        idle_poll: std::time::Duration,
    ) -> Option<LoadTask> {
        loop {
            match priority_rx.try_recv() {
                Ok(task) => return Some(task),
                Err(mpsc::TryRecvError::Disconnected) => return None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            // Фон пуст или отключён — не страшно: живём, пока жив priority.
            if let Ok(task) = bg_rx.try_recv() {
                return Some(task);
            }
            match priority_rx.recv_timeout(idle_poll) {
                Ok(task) => return Some(task),
                Err(mpsc::RecvTimeoutError::Disconnected) => return None,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
    // Дренирование: берём последнюю задачу Photo из канала, пропуская промежуточные.
    // 4.6a [CONC] — возвращает также сколько задач было пропущено (видимость
    // глубины очереди в логах).
    fn drain_to_last(
        rx: &mpsc::Receiver<LoadTask>,
        mut index: i32,
        mut path: std::path::PathBuf,
        mut generation: u64,
        mut skip_depth: u32,
    ) -> (i32, std::path::PathBuf, u64, u32, usize) {
        let mut skipped = 0usize;
        while let Ok(next) = rx.try_recv() {
            skipped += 1;
            if let LoadTask::Photo {
                index: i,
                path: p,
                generation: g,
                skip_depth: d,
            } = next
            {
                index = i;
                path = p;
                generation = g;
                skip_depth = d;
            }
            // Preload-задачи игнорируем при дренировании (они низкого приоритета).
        }
        (index, path, generation, skip_depth, skipped)
    }

    /// 4.3i — контекст вьюера: всё состояние показа фото в одном месте
    /// (KISS/DRY): список фото, кэши, поколение, направление, канал воркеру.
    /// ЕДИНЫЙ переход switch_to_photo: одно поколение на переход делят
    /// миниатюра (мгновенно) и чёткое фото — обе записи идут в ОДИН слой
    /// viewer-image и сверяются с одним поколением, поэтому два разных
    /// кадра физически не могут показаться одновременно.
    struct ViewerCtx {
        photos: Arc<Mutex<Vec<Photo>>>,
        ui_weak: slint::Weak<GalleryScreen>,
        cache: Arc<Mutex<ViewerCache>>,
        image_cache: ImageCache,
        generation: Arc<AtomicU64>,
        last_direction: Arc<AtomicI32>,
        /// 4.6b — приоритетный канал: ТОЛЬКО текущий кадр (Photo). Воркер
        /// опустошает его первым — сосед больше не встаёт перед кадром,
        /// который пользователь смотрит прямо сейчас.
        priority_tx: mpsc::Sender<LoadTask>,
        /// 4.6b — фоновый канал: предзагрузка соседей (без приоритета).
        bg_tx: mpsc::Sender<LoadTask>,
        /// 4.4a — режим листания текущего проекта (edge/cycle/random).
        /// Меняется при открытии сессии и из панели ⚙; UI-поток — RefCell ок.
        nav_mode: Rc<RefCell<NavMode>>,
        /// 4.4c — перемешанный порядок индексов для random-режима.
        /// Строится ОДИН раз при включении random (биекция 0..count);
        /// None в edge/cycle. Порядок фиксирован → «назад» честно
        /// возвращает предыдущее просмотренное фото.
        shuffle_order: RefCell<Option<Vec<i32>>>,
        /// 4.4c — текущая ПОЗИЦИЯ в перемешанном списке (не индекс фото!).
        shuffle_pos: Cell<i32>,
        /// 4.7a-fix2 — глобальные настройки (нужны переходу для анимации:
        /// галка «зациклить» читается аниматором на каждом проходе).
        settings: Arc<crate::settings::GlobalSettings>,
        /// 4.4c-3 — индексы битых файлов (воркер помечает при ошибке декода).
        /// Навигация всех режимов их ПРОПУСКАЕТ: random больше не попадает в
        /// битые серии (миниатюра битого не мелькает, скип не уводит на другой
        /// кадр). UI-поток — RefCell ок; растёт по ходу просмотра.
        broken: Rc<RefCell<std::collections::HashSet<i32>>>,
        /// 4.7a-fix2 — (index, generation) ТЕКУЩЕЙ играющей анимации.
        /// Аниматор — ЕДИНСТВЕННЫЙ писатель viewer-image, пока играет:
        /// статичные ветки (миниатюра, кэш Photo из request_load) для ЭТОГО
        /// перехода слой не трогают — иначе кэш перетирает играющие кадры
        /// первым статичным («GIF замер»). Arc<Mutex> — флаг живёт в
        /// потоке-аниматоре (Send), читают статичные писатели (UI-поток).
        anim_playing: Arc<Mutex<Option<(i32, u64)>>>,
        /// 4.7c — LRU-кэш декодированных кадров (по пути, ≤400 МБ):
        /// повторное открытие GIF — мгновенно, без повторного декода
        /// сотен кадров. Arc<Mutex> — доступ из потока-декодера (Send).
        anim_cache: Arc<Mutex<AnimFramesCache>>,
        /// 4.7d — состояние проигрывания (мост UI ⇄ аниматор): пауза,
        /// скорость, перемотка. Атомики — без блокировок, Send.
        playback: Arc<AnimPlayback>,
    }

    impl ViewerCtx {
        /// 4.4c — сменить режим листания. При включении random строит
        /// перемешанный список (один раз — порядок фиксируется, «назад»
        /// честный); при выключении — сбрасывает. Из on_open_session и ⚙.
        fn set_nav(&self, mode: NavMode) {
            *self.nav_mode.borrow_mut() = mode;
            match mode {
                NavMode::Random => {
                    let count = lock_or_recover(&self.photos).len() as i32;
                    let order = shuffled_order(count);
                    // Текущее фото — стартовая позиция, если оно в списке.
                    let cur = self
                        .ui_weak
                        .upgrade()
                        .map(|w| w.get_viewer_index())
                        .unwrap_or(0);
                    let start = order.iter().position(|&i| i == cur).unwrap_or(0) as i32;
                    *self.shuffle_order.borrow_mut() = Some(order);
                    self.shuffle_pos.set(start);
                }
                NavMode::Edge | NavMode::Cycle => {
                    *self.shuffle_order.borrow_mut() = None;
                    self.shuffle_pos.set(0);
                }
            }
        }

        /// 4.7a-fix2 — играет ли анимация для ЭТОГО перехода (index+gen)?
        /// Пока аниматор владеет слоем, статичные писатели (миниатюра,
        /// кэш-ветки request_load) для этого перехода слой не трогают —
        /// иначе кэш перетирает играющие кадры первым статичным кадром.
        fn anim_owns_layer(&self, index: i32, my_gen: u64) -> bool {
            matches!(
                *lock_or_recover(&self.anim_playing),
                Some((i, g)) if i == index && g == my_gen
            )
        }

        /// ЕДИНЫЙ переход к фото (4.3i): инкрементирует поколение, мгновенно
        /// пишет миниатюру в единый viewer-image и запрашивает чёткое фото
        /// ТОЙ ЖЕ генерацией. Открытие, next/prev и скип используют только
        /// эту схему — она существует в одном месте (DRY), и все записи в
        /// слой сверяются с одним поколением.
        /// 4.4c-3 — show_thumb=false для кандидата скипа: не показываем
        /// миниатюру, пока не знаем, что файл годный (иначе миниатюра битого
        /// мелькала перед уходом на другой кадр).
        fn switch_to_photo(&self, index: i32, skip_depth: u32, show_thumb: bool) {
            let my_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            // 4.7d-fix2 — панель анимации прячется СРАЗУ на новом переходе:
            // раньше anim-active висел true от прошлого GIF и панель
            // оставалась на статичных фото. Аниматор включит её обратно,
            // только если реально запустится (кадров > 1).
            if let Some(w) = self.ui_weak.upgrade() {
                w.set_anim_active(false);
            }
            if show_thumb {
                self.show_thumbnail(index, my_gen);
            }
            self.request_load(index, my_gen, skip_depth);
            // 4.7a-fix2 — анимация — ответственность ПЕРЕХОДА, а не воркера:
            // раньше decode кадров жил в воркере, и при повторном открытии
            // GIF из кэша (кэш-хит минует воркер) аниматор НЕ запускался —
            // «GIF замер на статике». Теперь: если файл потенциально
            // анимированный (по расширению), спавним поток-декодер кадров;
            // он сам решит (0-1 кадр = статика = ничего) и запустит
            // аниматор. Поколение сверяется на каждом шаге — листание
            // убивает поток мгновенно.
            let path_opt = {
                let p = lock_or_recover(&self.photos);
                p.get(index as usize).map(|ph| ph.path.clone())
            };
            if let Some(path) = path_opt {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .unwrap_or_default();
                // 4.7b-1 — ВИДЕО: показать первый кадр как превью
                // (проигрывание — 4.7b-2). Декод в фоне: COM MF не
                // трогаем в UI-потоке.
                if matches!(ext.as_str(), "mp4" | "webm" | "mov" | "mkv" | "avi") {
                    let gen = self.generation.clone();
                    let ui_weak = self.ui_weak.clone();
                    let idx = index;
                    let video_path = path.clone();
                    std::thread::spawn(move || {
                        if gen.load(Ordering::SeqCst) != my_gen {
                            return;
                        }
                        match crate::media::first_frame_rgba(&video_path) {
                            Ok((data, w, h)) => {
                                let _ = slint::invoke_from_event_loop(move || {
                                    // Двойная сверка (как у воркера).
                                    if gen.load(Ordering::SeqCst) != my_gen {
                                        return;
                                    }
                                    if let Some(w_ui) = ui_weak.upgrade() {
                                        if w_ui.get_viewer_index() != idx {
                                            return;
                                        }
                                        let mut buf =
                                            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(
                                                w, h,
                                            );
                                        let pixels = buf.make_mut_slice();
                                        for (dst, chunk) in
                                            pixels.iter_mut().zip(data.as_chunks::<4>().0)
                                        {
                                            dst.r = chunk[0];
                                            dst.g = chunk[1];
                                            dst.b = chunk[2];
                                            dst.a = chunk[3];
                                        }
                                        w_ui.set_viewer_img_w(w as f32);
                                        w_ui.set_viewer_img_h(h as f32);
                                        w_ui.set_viewer_image(slint::Image::from_rgba8(buf));
                                    }
                                });
                            }
                            Err(e) => log::warn!("video: {e}"),
                        }
                    });
                }
                if matches!(ext.as_str(), "gif" | "png" | "apng" | "webp") {
                    let gen = self.generation.clone();
                    let ui_weak = self.ui_weak.clone();
                    let settings = self.settings.clone();
                    let anim_playing = self.anim_playing.clone();
                    let anim_cache = self.anim_cache.clone();
                    let playback = self.playback.clone();
                    let idx = index;
                    std::thread::spawn(move || {
                        // Поколение уже ушло — не декодируем зря.
                        if gen.load(Ordering::SeqCst) != my_gen {
                            return;
                        }
                        // 4.7c — сначала КЭШ: повторное открытие того же GIF
                        // стартует аниматор мгновенно, без декода сотен
                        // кадров. Клоны кадров из кэша — аниматор владеет
                        // своей копией (кэш не отдаёт ссылки наружу).
                        let cached = lock_or_recover(&anim_cache).get(&path);
                        let frames = match cached {
                            Some(frames) => {
                                log::debug!(
                                    "[ANIM] кэш кадров: ХИТ «{}» ({} кадров)",
                                    path.display(),
                                    frames.len()
                                );
                                frames
                            }
                            None => {
                                log::debug!(
                                    "[ANIM] кэш кадров: МИМО — декодирую «{}»",
                                    path.display()
                                );
                                match decode_animated_frames(&path) {
                                    Ok(frames) if !frames.is_empty() => {
                                        lock_or_recover(&anim_cache).put(path, frames.clone());
                                        frames
                                    }
                                    Ok(_) => return, // статика — не анимируем
                                    Err(e) => {
                                        log::debug!("анимация: кадры не декодировались: {e}");
                                        return;
                                    }
                                }
                            }
                        };
                        spawn_animator(
                            frames,
                            idx,
                            my_gen,
                            gen,
                            ui_weak,
                            settings,
                            anim_playing,
                            playback,
                        );
                    });
                }
            }
        }

        /// Мгновенно показывает миниатюру фото по индексу (моргание).
        /// 4.3i — единый слой: миниатюра пишется в viewer-image (второго слоя
        /// больше нет). Как любая запись в слой, сверяет поколение: устаревший
        /// вызов не перезапишет кадр более нового перехода.
        /// 4.4c-6 — миниатюра ищется по ПУТИ файла (photos[index].path) —
        /// ТОМУ ЖЕ ключу, по которому request_load грузит чёткое. Раньше брали
        /// thumbnails.row_data(index): модель галереи и photos — ДВА списка,
        /// которые расходятся при досскане/догрузке батчей → index указывал
        /// на РАЗНЫЕ фото («миниатюра одна, чёткое другое»). По пути —
        /// миниатюра и чёткое гарантированно одного файла.
        fn show_thumbnail(&self, index: i32, my_gen: u64) {
            use slint::Model; // trait Model::row_data
            if self.generation.load(Ordering::SeqCst) != my_gen {
                return; // переход неактуален — слой принадлежит новому поколению
            }
            // 4.7a-fix2 — аниматор владеет слоем этого перехода: не трогаем
            // (иначе миниатюра перетрёт играющий кадр GIF).
            if self.anim_owns_layer(index, my_gen) {
                return;
            }
            // 4.4c-3 — битые не показываем: их миниатюра «врёт» (файл не
            // декодируется), а скип сейчас уведёт на другой кадр.
            if self.broken.borrow().contains(&index) {
                return;
            }
            // Истина — путь из photos (его же грузит чёткое).
            let path = {
                let p = lock_or_recover(&self.photos);
                match p.get(index as usize) {
                    Some(ph) => ph.path.to_string_lossy().to_string(),
                    None => return, // индекс вне списка — нечего показывать
                }
            };
            if let Some(w) = self.ui_weak.upgrade() {
                let thumbs = w.get_thumbnails();
                // Ищем миниатюру по ПУТИ, не по индексу модели.
                let found = (0..thumbs.row_count())
                    .filter_map(|i| thumbs.row_data(i))
                    .find(|item| item.source_path == path.as_str());
                match found {
                    Some(item) => {
                        // Размеры для клампа зума: у миниатюры то же соотношение
                        // сторон, что у полного фото, поэтому contain-fit считается
                        // корректно ещё до прихода чёткого кадра (DPI сокращается).
                        let sz = item.image.size();
                        w.set_viewer_img_w(sz.width as f32);
                        w.set_viewer_img_h(sz.height as f32);
                        w.set_viewer_image(item.image);
                        log::debug!("[CONC] show: thumb idx={index} (по пути, gen={my_gen})");
                    }
                    // Миниатюры по пути нет (батч не дошёл) — чистим слой,
                    // чтобы не показывать кадр предыдущего перехода.
                    None => {
                        w.set_viewer_image(slint::Image::default());
                        log::debug!(
                            "[CONC] show: thumb idx={index} НЕТ в модели — слой очищен (gen={my_gen})"
                        );
                    }
                }
            }
        }

        /// Запросить показ чёткого фото по индексу (v7 — кэш готовых slint::Image
        /// и воркер). 4.3i — my_gen приходит снаружи (от switch_to_photo):
        /// одно поколение на переход делят миниатюра и чёткое фото — записи
        /// в единый слой сверяются с одним и тем же поколением. skip_depth
        /// протаскивается в задачу воркера (авто-скип битых файлов, 4.3e).
        /// 1) UI-кэш готовых slint::Image — МГНОВЕННО (дешёвый clone, без копирования).
        /// 2) Сырой RGBA в кэше — строим slint::Image ОДИН раз, сохраняем в UI-кэш,
        ///    показываем. При повторе — уже мгновенно из UI-кэша.
        /// 3) Мимо кэша — задача воркеру (1 поток).
        fn request_load(&self, index: i32, my_gen: u64, skip_depth: u32) {
            let idx = index as usize;
            let path = {
                let p = lock_or_recover(&self.photos);
                match p.get(idx) {
                    Some(ph) => ph.path.clone(),
                    None => return,
                }
            };
            // 1) UI-кэш готовых slint::Image — мгновенно, без копирования RGBA.
            if let Some(img) = self.image_cache.borrow().get(&path).cloned() {
                if self.generation.load(Ordering::SeqCst) == my_gen {
                    log::debug!(
                        "request_load: «{}» из UI-кэша Image — мгновенно",
                        path.display()
                    );
                    if let Some(w) = self.ui_weak.upgrade() {
                        // 4.4c-5 — ИНВАРИАНТ ЭКРАНА: пишем чёткое ТОЛЬКО если
                        // текущий viewer-index равен индексу задачи. Поколение
                        // сверяется выше, но его недостаточно: кэш-хиты разных
                        // индексов в одном поколении могли перебивать кадр.
                        if w.get_viewer_index() != index {
                            log::debug!(
                                "request_load(UI-кэш): индекс ушёл (задача={index}, текущий={}) — не пишем",
                                w.get_viewer_index()
                            );
                            return;
                        }
                        // 4.3g — размеры картинки для клампа «край в край» (contain-fit).
                        // size() — физические px; в формуле contain_scale DPI сокращается.
                        let sz = img.size();
                        w.set_viewer_img_w(sz.width as f32);
                        w.set_viewer_img_h(sz.height as f32);
                        w.set_viewer_image(img);
                        w.set_viewer_error("".into()); // 4.3e — успешный показ, гасим плашку.
                        log::debug!("[CONC] put: ui-cache idx={index} gen={my_gen}");
                    }
                    self.enqueue_preload(index);
                }
                return;
            }
            // 2) Сырой RGBA в кэше — строим slint::Image ОДИН раз, кэшируем, показываем.
            let cached_opt = lock_or_recover(&self.cache).get(&path).cloned();
            if let Some(cached) = cached_opt {
                if self.generation.load(Ordering::SeqCst) == my_gen {
                    log::debug!(
                        "request_load: «{}» из RGBA-кэша — строим Image",
                        path.display()
                    );
                    if let Some(w) = self.ui_weak.upgrade() {
                        // 4.4c-5 — ИНВАРИАНТ ЭКРАНА: та же сверка индекса.
                        if w.get_viewer_index() != index {
                            log::debug!(
                                "request_load(RGBA-кэш): индекс ушёл (задача={index}, текущий={}) — не пишем",
                                w.get_viewer_index()
                            );
                            return;
                        }
                        let (data, w_px, h_px) = cached;
                        let mut buf =
                            slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w_px, h_px);
                        let pixels = buf.make_mut_slice();
                        for (dst, chunk) in pixels.iter_mut().zip(data.as_chunks::<4>().0) {
                            dst.r = chunk[0];
                            dst.g = chunk[1];
                            dst.b = chunk[2];
                            dst.a = chunk[3];
                        }
                        let img = slint::Image::from_rgba8(buf);
                        // Сохраняем в UI-кэш (с ограничением размера).
                        let mut ic = self.image_cache.borrow_mut();
                        if ic.len() >= IMAGE_CACHE_MAX {
                            // Удаляем произвольный элемент (простейшая eviction).
                            if let Some(k) = ic.keys().next().cloned() {
                                ic.remove(&k);
                            }
                        }
                        ic.insert(path.clone(), img.clone());
                        // 4.3g — размеры картинки для клампа «край в край».
                        w.set_viewer_img_w(w_px as f32);
                        w.set_viewer_img_h(h_px as f32);
                        w.set_viewer_image(img);
                        w.set_viewer_error("".into()); // 4.3e — успешный показ, гасим плашку.
                        log::debug!("[CONC] put: rgba-cache idx={index} gen={my_gen}");
                    }
                    self.enqueue_preload(index);
                }
                return;
            }
            // 3) Мимо кэша — задача воркеру. Миниатюра уже в viewer-image
            //    (единый слой, 4.3i); индикатор «Загрузка» убран (бесполезен и залипал).
            let _ = self.priority_tx.send(LoadTask::Photo {
                index,
                path,
                generation: my_gen,
                skip_depth,
            });
        }

        /// 4.4c-4 — ставит задачи на предзагрузку соседей ПО ТЕКУЩЕМУ ПОРЯДКУ.
        /// 3 соседа в каждую сторону; в random — соседи по ПЕРЕМЕШАННОМУ
        /// списку (позиции shuffle_pos±1..3), не линейные index±1 (раньше
        /// линейные чужие кадры перебивали текущий в random).
        /// Сосед кладётся ТОЛЬКО в кэш — на экран не пишется (это делает
        /// LoadTask::Photo при реальном переходе).
        fn enqueue_preload(&self, index: i32) {
            const PRELOAD_RADIUS: i32 = 3;
            let count = lock_or_recover(&self.photos).len() as i32;
            // В random соседи — по перемешанному списку от текущей позиции.
            let (order, pos) = {
                let ord = self.shuffle_order.borrow();
                match ord.as_ref() {
                    Some(o) => (Some(o.to_vec()), self.shuffle_pos.get()),
                    None => (None, 0),
                }
            };
            let broken = self.broken.borrow().clone();
            for ni in
                preload_neighbors(index, count, PRELOAD_RADIUS, order.as_deref(), pos, &broken)
            {
                let path = {
                    let p = lock_or_recover(&self.photos);
                    match p.get(ni as usize) {
                        Some(ph) => ph.path.clone(),
                        None => continue,
                    }
                };
                // Уже в кэше — не шлём.
                if lock_or_recover(&self.cache).contains(&path) {
                    continue;
                }
                let _ = self.bg_tx.send(LoadTask::Preload { path });
            }
        }
    }

    // 4.3i — единый контекст вьюера: колбэки открытия, next/prev и скипа
    // работают только через него (Rc-клон в каждое замыкание) — состояние
    // в одном месте, схема перехода одна (DRY/KISS).
    let viewer_ctx = Rc::new(ViewerCtx {
        photos: photos.clone(),
        ui_weak: ui.as_weak(),
        cache: viewer_cache.clone(),
        image_cache: image_cache.clone(),
        generation: viewer_generation.clone(),
        last_direction: last_direction.clone(),
        priority_tx: priority_tx.clone(),
        bg_tx: bg_tx.clone(),
        nav_mode: Rc::new(RefCell::new(NavMode::Edge)), // 4.4a — до края по умолчанию
        shuffle_order: RefCell::new(None),              // 4.4c — строится при random
        shuffle_pos: Cell::new(0),
        broken: Rc::new(RefCell::new(std::collections::HashSet::new())), // 4.4c-3
        anim_playing: Arc::new(Mutex::new(None)), // 4.7a-fix2 — аниматор владеет слоем
        settings: global_settings.clone(),        // 4.7a-fix2 — анимация из перехода
        anim_cache: Arc::new(Mutex::new(AnimFramesCache::new(ANIM_CACHE_MAX_BYTES))), // 4.7c
        playback: Arc::new(AnimPlayback::new()),  // 4.7d — панель управления
    });

    /// Подпись режима ресайза для логов [CONC]: «1 поток» или «ultra N ядер».
    /// Читает атомики настроек на момент вызова (без блокировок).
    fn resize_mode_label(settings: &crate::settings::GlobalSettings) -> String {
        let (on, threads) = settings.snapshot();
        if on {
            format!("ultra {threads} ядер")
        } else {
            "1 поток".to_string()
        }
    }

    /// Загружает полное изображение по пути и возвращает сырые RGBA-данные (Send).
    /// Downscale до VIEWER_MAX_SIZE для скорости. Результат кэшируется.
    /// Декодирование обёрнуто в catch_unwind: битый/нестандартный файл не должен
    /// паниковать и отравлять мьютекс — ошибка возвращается как Err.
    /// 4.6c (Фаза 2): при включённой «Ультра-скорости» downscale идёт через
    /// fast_image_resize (SIMD) на N ядрах (rayon-пул с числом потоков из
    /// настройки); выключена — прежний однопоточный image::resize (KISS:
    /// тяжёлый путь меняется ТОЛЬКО при явном желании пользователя).
    fn load_full_image(
        path: std::path::PathBuf,
        cache: Arc<Mutex<ViewerCache>>,
        settings: &crate::settings::GlobalSettings,
    ) -> Result<(Vec<u8>, u32, u32), String> {
        log::trace!("load_full_image: запрос «{}»", path.display());
        // Проверяем кэш. ВАЖНО: guard освобождается сразу после клонирования —
        // не держим блокировку дольше нужного (правило: не вызывать функции,
        // которые сами берут тот же Mutex, пока держим guard).
        let cached_opt = lock_or_recover(&cache).get(&path).cloned();
        if let Some(cached) = cached_opt {
            log::trace!("load_full_image: есть в кэше «{}»", path.display());
            return Ok(cached);
        }
        log::debug!("load_full_image: мимо кэша, декодирую «{}»", path.display());
        let start = std::time::Instant::now();
        // Декодирование в панико-безопасной обёртке. image::open может паниковать
        // на некоторых файлах; ловим панику, чтобы не отравить мьютекс.
        let decode = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            image::open(&path).map_err(|e| format!("Ошибка загрузки: {e}"))
        }))
        .map_err(|_| format!("Паника при декодировании: {}", path.display()))?;
        let img = decode?;
        // Downscale, если фото больше максимума. 4.6c — ДВА режима:
        // «Ультра-скорость» ВКЛ → fast_image_resize на N ядрах (SIMD);
        // ВЫКЛ (по умолчанию) → прежний однопоточный Triangle.
        let (ultra_on, ultra_threads) = settings.snapshot();
        let img = if img.width() > VIEWER_MAX_SIZE || img.height() > VIEWER_MAX_SIZE {
            log::debug!(
                "load_full_image: downscale {}x{} → ≤{VIEWER_MAX_SIZE} ({} режим)",
                img.width(),
                img.height(),
                if ultra_on { "ultra" } else { "1 поток" }
            );
            if ultra_on {
                resize_ultra(img, ultra_threads, settings)?
            } else {
                img.resize(
                    VIEWER_MAX_SIZE,
                    VIEWER_MAX_SIZE,
                    image::imageops::FilterType::Triangle,
                )
            }
        } else {
            img
        };
        let rgba = img.to_rgba8();
        let (w, h) = rgba.dimensions();
        let data = rgba.into_raw();
        log::debug!(
            "load_full_image: декодировано «{}» {w}x{h} за {:?}",
            path.display(),
            start.elapsed()
        );
        // Сохраняем в кэш (короткая блокировка).
        lock_or_recover(&cache).put(path, (data.clone(), w, h));
        Ok((data, w, h))
    }

    /// 4.7a — верхний лимит кадров анимации в памяти. GIF на 1000 кадров
    /// 2560×2560 — это гигабайты RGBA; без лимта можно уронить ноут по
    /// памяти. 200 кадров хватает практически любой осмысленной анимации;
    /// длинные обрежутся (лучше обрезать, чем OOM).
    const ANIM_MAX_FRAMES: usize = 200;

    /// 4.7a-fix4 — квант сна аниматора: пауза кадра дробится на куски
    /// ≤30 мс, после каждого куска сверяется поколение. Старый аниматор
    /// умирает за ≤30 мс после нового перехода — зомби-потоки, каждый
    /// со своим invoke в очередь UI, не копятся.
    const ANIM_SLEEP_QUANTUM_MS: u64 = 30;

    /// 4.7a — декодирует ВСЕ кадры анимированного файла (GIF/APNG/WEBP)
    /// через AnimationDecoder крейта image. Не-анимированный файл или
    /// формат без анимации → Ok(vec![]) (пусто = показывать статично,
    /// как раньше). Ошибка декодирования НЕ фатальна: Err → файл
    /// покажется обычным путём через load_full_image (фолбэк).
    fn decode_animated_frames(path: &std::path::Path) -> Result<Vec<AnimFrame>, String> {
        use image::AnimationDecoder;
        // Открываем по расширению: image::open не отдаёт AnimationDecoder,
        // нужен конкретный кодек. GIF — самое частое; APNG и WEBP — бонус.
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        let file = std::fs::File::open(path).map_err(|e| format!("анимация: открыть файл: {e}"))?;
        let reader = std::io::BufReader::new(file);
        let frames_iter = match ext.as_str() {
            "gif" => {
                let dec = image::codecs::gif::GifDecoder::new(reader)
                    .map_err(|e| format!("анимация: GIF декодер: {e}"))?;
                dec.into_frames()
            }
            "png" | "apng" => {
                // APNG: PngDecoder::apng() — публичный конструктор обёртки
                // (возвращает Result). Обычный PNG отдаст 0-1 кадров —
                // статика, отфильтруется проверкой len <= 1 ниже.
                let png = image::codecs::png::PngDecoder::new(reader)
                    .map_err(|e| format!("анимация: PNG декодер: {e}"))?;
                let dec = png
                    .apng()
                    .map_err(|e| format!("анимация: APNG обёртка: {e}"))?;
                dec.into_frames()
            }
            "webp" => {
                let dec = image::codecs::webp::WebPDecoder::new(reader)
                    .map_err(|e| format!("анимация: WEBP декодер: {e}"))?;
                dec.into_frames()
            }
            _ => return Ok(vec![]), // не анимированный формат — статика
        };
        let mut frames = Vec::new();
        for f in frames_iter {
            let f = f.map_err(|e| format!("анимация: кадр: {e}"))?;
            // 4.7a-fix4 — единицы дроби УЖЕ миллисекунды (см. anim_delay_ms):
            // формула с ×1000 ошибочно трактовала их как секунды.
            let delay_ms = anim_delay_ms(f.delay().numer_denom_ms());
            let rgba = f.into_buffer();
            let (w, h) = rgba.dimensions();
            frames.push(AnimFrame {
                data: rgba.into_raw(),
                w,
                h,
                delay_ms,
            });
            if frames.len() >= ANIM_MAX_FRAMES {
                log::warn!("анимация: {ANIM_MAX_FRAMES} кадров — обрезаем (защита памяти)");
                break;
            }
        }
        // 1 кадр = не анимация (статичный PNG через APNG-путь и т.п.).
        if frames.len() <= 1 {
            return Ok(vec![]);
        }
        // 4.7a-fix4 — сводка задержек в лог: единицы и длительность цикла
        // видны сразу, «замер» диагностируется без ручного пересчёта.
        let sum_ms: u64 = frames.iter().map(|f| f.delay_ms).sum();
        let dmin = frames.iter().map(|f| f.delay_ms).min().unwrap_or(0);
        let dmax = frames.iter().map(|f| f.delay_ms).max().unwrap_or(0);
        log::info!(
            "анимация: {} кадров, {}x{}, задержки {}..{} мс, цикл ~{} мс",
            frames.len(),
            frames.first().map(|f| f.w).unwrap_or(0),
            frames.first().map(|f| f.h).unwrap_or(0),
            dmin,
            dmax,
            sum_ms
        );
        Ok(frames)
    }

    /// 4.7a/4.7d — поток-аниматор: проигрывает кадры в UI-поток. Живёт в
    /// ОТДЕЛЬНОМ потоке (воркер не блокируется), между кадрами спит
    /// delay_ms. Каждый кадр отдаётся через invoke_from_event_loop со
    /// сверкой ПОКОЛЕНИЯ (как у воркера): пользователь ушёл на другое
    /// фото → поколение сменилось → аниматор молча завершается, чужие
    /// кадры на экран не попадут (инвариант «кадр = viewer-index»
    /// сохранён — кадр анимации пишется только когда поколение живо).
    /// 4.7d — аниматор УПРАВЛЯЕМЫЙ: на каждом шаге читает AnimPlayback
    /// (пауза/скорость/перемотка из панели) и пишет текущий кадр (бегунок
    /// перемотки двигается за анимацией). «Зациклить» — иконка-тумблер
    /// на панели (4.7d), читается на каждом проходе.
    // 8 аргументов — всё, что нужно потоку; структура лишь добавила бы
    // шум (осознанное исключение, KISS).
    #[allow(clippy::too_many_arguments)]
    fn spawn_animator(
        frames: Vec<AnimFrame>,
        index: i32,
        my_gen: u64,
        generation: Arc<AtomicU64>,
        ui_weak: slint::Weak<GalleryScreen>,
        settings: Arc<crate::settings::GlobalSettings>,
        anim_playing: Arc<Mutex<Option<(i32, u64)>>>,
        playback: Arc<AnimPlayback>,
    ) {
        std::thread::spawn(move || {
            log::debug!(
                "[ANIM] аниматор запущен: idx={index} gen={my_gen} кадров={}",
                frames.len()
            );
            let total = frames.len();
            // 4.7a-fix2 — заявляем владение слоем (в UI-потоке): пока
            // anim_playing == (index, my_gen), статичные писатели не
            // трогают viewer-image. Чистим при любом выходе.
            // 4.7d — заодно включаем панель анимации (anim-active),
            // максимум бегунка и сброс позиции/паузы (скорость живёт).
            {
                let ap = anim_playing.clone();
                let ui_weak_start = ui_weak.clone();
                let total_i32 = total as i32;
                let _ = slint::invoke_from_event_loop(move || {
                    *lock_or_recover(&ap) = Some((index, my_gen));
                    if let Some(w) = ui_weak_start.upgrade() {
                        w.set_anim_active(true);
                        w.set_anim_total(total_i32);
                        w.set_anim_frame(0);
                        w.set_anim_paused(false);
                    }
                });
            }
            playback.set_total(total);
            playback.set_frame(0);
            playback.set_paused(false);
            let mut cur: usize = 0;
            // 4.7a-fix4 — единая точка выхода: после break 'anim слой
            // освобождается ОДИН раз (DRY). 4.7d-fix — выход ЕДИНСТВЕННЫЙ:
            // смена поколения (уход с фото); конец прохода теперь НЕ
            // убивает поток, а ставит автопаузу — флага причины больше нет.
            'anim: loop {
                // 4.7d — перемотка: забрать запрос (один раз, атомарно),
                // прыгнуть на кадр. Кламп по числу кадров.
                if let Some(seek_to) = playback.take_seek() {
                    cur = seek_to.clamp(0, total as i32 - 1) as usize;
                }
                // Поколение сменилось — пользователь ушёл, стоп.
                // 4.7a-fix3 — сверка ДО показа кадра: первый кадр
                // проверяется мгновенно после старта аниматора.
                if generation.load(Ordering::SeqCst) != my_gen {
                    break 'anim;
                }
                let f = &frames[cur];
                let gen = generation.clone();
                // 4.7d-fix — клон кадра под СВОИМ именем: затенение
                // внешнего ui_weak ломало автопаузу ниже (borrow after move).
                let ui_weak_frame = ui_weak.clone();
                let (data, w, h) = (f.data.clone(), f.w, f.h);
                // 4.8 — покадровый лог на TRACE: виден только на уровне
                // «Подробный» (для обычного пользователя лог чистый).
                log::trace!(
                    "[ANIM] кадр {}/{} idx={index} gen={my_gen} (пауза {} мс)",
                    cur + 1,
                    total,
                    f.delay_ms
                );
                playback.set_frame(cur);
                let _ = slint::invoke_from_event_loop(move || {
                    // Двойная сверка (как у воркера): поколение + индекс.
                    let cur_gen = gen.load(Ordering::SeqCst);
                    if cur_gen != my_gen {
                        log::debug!("[ANIM] кадр отсечён в UI (поколение {cur_gen} ≠ {my_gen})");
                        return;
                    }
                    if let Some(w_ui) = ui_weak_frame.upgrade() {
                        if w_ui.get_viewer_index() != index {
                            log::trace!("[ANIM] кадр отсечён в UI (индекс ушёл)");
                            return; // индекс ушёл — не показываем
                        }
                        let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w, h);
                        let pixels = buf.make_mut_slice();
                        for (dst, chunk) in pixels.iter_mut().zip(data.as_chunks::<4>().0) {
                            dst.r = chunk[0];
                            dst.g = chunk[1];
                            dst.b = chunk[2];
                            dst.a = chunk[3];
                        }
                        w_ui.set_viewer_img_w(w as f32);
                        w_ui.set_viewer_img_h(h as f32);
                        w_ui.set_viewer_image(slint::Image::from_rgba8(buf));
                        // 4.7d — двигаем бегунок перемотки за анимацией.
                        w_ui.set_anim_frame(cur as i32);
                        log::trace!("[ANIM] кадр НА ЭКРАНЕ idx={index}");
                    }
                });
                // 4.7a-fix3/fix4 — пауза ПОСЛЕ показа, квантами ≤30 мс со
                // сверкой поколения: старый аниматор умирает за ≤30 мс.
                // 4.7d — пауза делится на скорость (×2 = вдвое быстрее),
                // а is_paused крутит цикл вхолостую (кадр висит).
                let mut left = effective_delay_ms(f.delay_ms, playback.speed_tenths());
                while left > 0 {
                    if playback.is_paused() {
                        // Пауза: спим тиками 30 мс, кадр не меняется.
                        std::thread::sleep(std::time::Duration::from_millis(ANIM_SLEEP_QUANTUM_MS));
                        // Пауза не отменяет сверки поколения и перемотки:
                        // ушёл с фото / потянул бегунок — реагируем.
                        if generation.load(Ordering::SeqCst) != my_gen {
                            break 'anim;
                        }
                        if let Some(seek_to) = playback.take_seek() {
                            cur = seek_to.clamp(0, total as i32 - 1) as usize;
                            break; // прыжок — сразу показать новый кадр
                        }
                        continue;
                    }
                    let quantum = left.min(ANIM_SLEEP_QUANTUM_MS);
                    std::thread::sleep(std::time::Duration::from_millis(quantum));
                    if generation.load(Ordering::SeqCst) != my_gen {
                        break 'anim;
                    }
                    // Перемотка во время паузы кадра — прерываем ожидание.
                    if let Some(seek_to) = playback.take_seek() {
                        cur = seek_to.clamp(0, total as i32 - 1) as usize;
                        break;
                    }
                    left -= quantum;
                }
                // Прыжок перемотки прервал паузу кадра — без инкремента.
                if playback.take_seek().is_some() {
                    continue;
                }
                // 4.7d-fix — конец прохода без зацикливания: НЕ умираем —
                // встаём на АВТОПАУЗУ на последнем кадре (панель жива,
                // кнопка показывает ▶, можно перемотать/продолжить).
                // Поток умирает ТОЛЬКО по смене поколения (уход с фото).
                if cur + 1 >= total {
                    if !settings.loop_animation_on() {
                        playback.set_paused(true);
                        let ui_weak_pause = ui_weak.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            if let Some(w) = ui_weak_pause.upgrade() {
                                w.set_anim_paused(true);
                            }
                        });
                        log::debug!(
                            "[ANIM] конец прохода — автопауза на кадре {}/{}",
                            cur + 1,
                            total
                        );
                        // cur остаётся на последнем кадре; цикл продолжит
                        // крутиться по паузе (тики 30мс) — ждёт ▶ или уход.
                    } else {
                        cur = 0; // по кругу
                    }
                } else {
                    cur += 1;
                }
            }
            // Единая точка выхода: освобождение слоя.
            // 4.7d-fix — сверка поколения: СТАРЫЙ аниматор (его поколение
            // уже неактуально) НЕ гасит панель нового перехода — иначе
            // панель моргала при листании (старый писал false ПОСЛЕ
            // того как новый написал true).
            log::debug!("[ANIM] аниматор остановлен (поколение ушло): idx={index}");
            let ap = anim_playing.clone();
            let gen_exit = generation.clone();
            let _ = slint::invoke_from_event_loop(move || {
                *lock_or_recover(&ap) = None;
                // Гасим панель ТОЛЬКО если наше поколение всё ещё актуально.
                if gen_exit.load(Ordering::SeqCst) != my_gen {
                    return; // ушёл на другое фото — панель принадлежит новому
                }
                if let Some(w) = ui_weak.upgrade() {
                    w.set_anim_active(false);
                    w.set_anim_paused(false);
                }
            });
        });
    }

    /// 4.6c (Фаза 2) — ускоренный downscale для «Ультра-скорости»:
    /// fast_image_resize (SIMD-инструкции процессора) на N ядрах.
    /// Число потоков задаётся ЯВНО пулом rayon — крейт берёт потоки из
    /// текущего пула (current_num_threads), поэтому ограничиваем
    /// ThreadPoolBuilder'ом ровно настройкой пользователя, не хватая
    /// все ядра системы без спроса.
    /// Фильтр Bilinear ≈ прежний Triangle (быстрый, мягкий) — качество
    /// картинки не меняется, меняется только скорость.
    /// Любая ошибка крейта — Err (файл покажется через обычный путь).
    fn resize_ultra(
        img: image::DynamicImage,
        threads: usize,
        settings: &crate::settings::GlobalSettings,
    ) -> Result<image::DynamicImage, String> {
        let threads = threads.max(1);
        // 1) image → сырой RGBA8 (fast_image_resize работает с буферами).
        let rgba = img.to_rgba8();
        let (src_w, src_h) = rgba.dimensions();
        // 2) Целевой размер — как у image::resize: вписать в квадрат
        //    VIEWER_MAX_SIZE с сохранением пропорций.
        let (dst_w, dst_h) = fit_into_max(src_w, src_h, VIEWER_MAX_SIZE);
        // 4.6c-fix2 [CONC] — видимость: пул создан именно на N потоков
        // (настройка), картинка такого-то размера. Реальную занятость ядер
        // показывает ОС (Диспетчер задач → ЦП), приложение её не знает.
        log::debug!(
            "[CONC] ultra: пул {threads} потоков, картинка {src_w}x{src_h} → {dst_w}x{dst_h}"
        );
        // 4.6c-fix3 — детальная статистика ТОЛЬКО под галкой «Отладка»
        // (ВЫКЛ по умолчанию у всех): сколько полос rayon реально возьмёт
        // = сколько потоков максимум будет молотить эту картинку. Формула
        // та же, что внутри fast_image_resize: площадь/1024, но не больше
        // высоты и не больше пула. Для мелких картинок полос меньше —
        // часть ядер простаивает (это честно видно в логе).
        if settings.debug_stats_on() {
            let area_parts = (src_w as u64 * src_h as u64) / 1024;
            let strips = area_parts.min(src_h as u64).min(threads as u64).max(1);
            log::debug!(
                "[CONC] ultra: работало полос {strips} (из пула {threads}) — столько потоков молотило картинку"
            );
        }
        // 3) Собираем исходник/приёмник крейта из сырых буферов.
        let src = fast_image_resize::images::Image::from_vec_u8(
            src_w,
            src_h,
            rgba.into_raw(),
            fast_image_resize::PixelType::U8x4,
        )
        .map_err(|e| format!("ultra: исходный буфер: {e}"))?;
        let mut dst =
            fast_image_resize::images::Image::new(dst_w, dst_h, fast_image_resize::PixelType::U8x4);
        let options = fast_image_resize::ResizeOptions::new()
            .resize_alg(fast_image_resize::ResizeAlg::Convolution(
                fast_image_resize::FilterType::Bilinear,
            ))
            .use_alpha(false);
        // 4) Ресайз в пуле rayon с ЯВНЫМ числом потоков (настройка).
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .map_err(|e| format!("ultra: пул потоков: {e}"))?;
        pool.install(|| {
            fast_image_resize::Resizer::new()
                .resize(&src, &mut dst, &options)
                .map_err(|e| format!("ultra: ресайз: {e}"))
        })?;
        // 5) Буфер → обратно в image::DynamicImage (RGBA8).
        let buf = dst.into_vec();
        let out = image::RgbaImage::from_raw(dst_w, dst_h, buf)
            .ok_or_else(|| "ultra: буфер результата не сошёлся по размеру".to_string())?;
        Ok(image::DynamicImage::ImageRgba8(out))
    }

    /// Целевые размеры вписывания в квадрат max×max с сохранением
    /// пропорций (та же арифметика, что у image::resize с bounding box).
    fn fit_into_max(src_w: u32, src_h: u32, max: u32) -> (u32, u32) {
        if src_w.max(src_h) <= max {
            return (src_w, src_h); // и так влезает — ресайз не нужен
        }
        let scale = max as f64 / src_w.max(src_h) as f64;
        let w = (src_w as f64 * scale).round() as u32;
        let h = (src_h as f64 * scale).round() as u32;
        // Защита от нуля при экстремальных пропорциях.
        (w.max(1), h.max(1))
    }

    // (v6) load_viewer_image и preload_neighbors удалены — их заменили
    // воркер-поток (цикл приёма LoadTask) и enqueue_preload (шлёт в канал).
    // Это убирает std::thread::spawn на каждую задачу — теперь всегда 1 поток.

    /// 4.4b — строит карточку галереи из миниатюры (RGBA → SharedPixelBuffer
    /// → slint::Image). Раньше этот код копировался в 3 местах (scan_and_show,
    /// открытие сессии, досскан) — теперь одно место (DRY).
    fn thumb_item_from(t: crate::core::models::Thumbnail) -> ThumbItem {
        let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(t.width, t.height);
        let pixels = buf.make_mut_slice();
        for (dst, chunk) in pixels.iter_mut().zip(t.data.as_chunks::<4>().0) {
            dst.r = chunk[0];
            dst.g = chunk[1];
            dst.b = chunk[2];
            dst.a = chunk[3];
        }
        ThumbItem {
            file_name: t
                .source_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
                .into(),
            source_path: t.source_path.to_string_lossy().to_string().into(),
            image: slint::Image::from_rgba8(buf),
        }
    }

    /// 4.7e — проходит ли миниатюра фильтр расширений (по ПУТИ файла).
    /// Чистая функция: (галочки из UI) × (путь) → да/нет.
    fn passes_filter(w: &GalleryScreen, path: &str) -> bool {
        let ext = path
            .rsplit('.')
            .next()
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "jpg" | "jpeg" => w.get_filter_jpg(),
            "png" => w.get_filter_png(),
            "gif" => w.get_filter_gif(),
            "bmp" => w.get_filter_bmp(),
            "webp" => w.get_filter_webp(),
            "mp4" => w.get_filter_mp4(),
            "webm" => w.get_filter_webm(),
            "mov" => w.get_filter_mov(),
            "mkv" => w.get_filter_mkv(),
            "avi" => w.get_filter_avi(),
            _ => true, // неизвестное расширение — показываем (не теряем)
        }
    }

    /// 4.7e — пересобрать ОТОБРАЖАЕМУЮ модель из ПОЛНОЙ (Slint-модель
    /// full-thumbnails) по фильтру. Вызывается при смене галочек.
    fn rebuild_filtered_thumbnails(w: &GalleryScreen) {
        use slint::Model;
        let full = w.get_full_thumbnails();
        let filtered: Vec<ThumbItem> = full
            .iter()
            .filter(|it| passes_filter(w, it.source_path.as_ref()))
            .collect();
        // Заменяем модель целиком — сетка перерисуется.
        w.set_thumbnails(slint::ModelRc::new(slint::VecModel::from(filtered)));
    }

    /// 4.4b — ПОРЦИОННАЯ загрузка миниатюр в галерею (чинит «тупит 30-60 сек»
    /// на 10k фото). Первый батч (FIRST_BATCH=200) заменяет модель — сетка
    /// оживает сразу; остальные добавляются батчами по BATCH=150 в фоне
    /// через invoke_from_event_loop, между батчами UI успевает отрисовать
    /// кадр (не давится одним куском на 10k карточек).
    /// gallery_gen — поколение галереи: батчи старого проекта отменяются,
    /// если открыли другой (иначе чужие миниатюры подмешаются в сетку).
    /// Хэндл VecModel (не-Send) живёт в UI-потоке — фон добирается до него
    /// через invoke_from_event_loop, где берёт модель из UI и downcast'ит.
    /// 4.7e — батчи пишут в ПОЛНУЮ Slint-модель (full-thumbnails);
    /// отображаемая (thumbnails) — только прошедшие фильтр.
    fn load_thumbs_batched(
        photos: Vec<Photo>,
        thumbs: &Arc<FileSystemThumbnailProvider>,
        ui_weak: &slint::Weak<GalleryScreen>,
        gallery_gen: &Arc<AtomicU64>,
        status_prefix: String,
    ) {
        const FIRST_BATCH: usize = 200;
        const BATCH: usize = 150;
        let my_gen = gallery_gen.fetch_add(1, Ordering::SeqCst) + 1;
        let total = photos.len();
        let thumbs = thumbs.clone();
        let ui_weak = ui_weak.clone();
        let gallery_gen = gallery_gen.clone();
        std::thread::spawn(move || {
            // Первый батч 200 — сетка оживает сразу; остальные по 150.
            // ВАЖНО: срез [..200] даёт РОВНО один кусок (chunks не выйдет за
            // границу среза) — иначе фото 200+ попали бы в модель дважды.
            let rest_start = FIRST_BATCH.min(total);
            let mut done = 0usize;
            for (batch_i, chunk) in photos[..rest_start]
                .chunks(FIRST_BATCH)
                .chain(photos[rest_start..].chunks(BATCH))
                .enumerate()
            {
                // Миниатюры (чистый RGBA, Send) — строим в фоне.
                let results = thumbs.thumbnails(chunk);
                let raw: Vec<crate::core::models::Thumbnail> =
                    results.into_iter().flatten().collect();
                done += chunk.len();
                let is_first = batch_i == 0;
                let prefix = status_prefix.clone();
                let ui_weak = ui_weak.clone();
                let gallery_gen = gallery_gen.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    // Открыли другой проект/папку — батчи старого отменяем.
                    if gallery_gen.load(Ordering::SeqCst) != my_gen {
                        return;
                    }
                    if let Some(w) = ui_weak.upgrade() {
                        // ThumbItem (не-Send) строим ЗДЕСЬ, в UI-потоке.
                        let items: Vec<ThumbItem> = raw.into_iter().map(thumb_item_from).collect();
                        use slint::Model;
                        // 4.7e — первый батч: полная модель = срез,
                        // отображаемая = отфильтрованная.
                        if is_first {
                            w.set_full_thumbnails(slint::ModelRc::new(slint::VecModel::from(
                                items.clone(),
                            )));
                            let filtered: Vec<ThumbItem> = items
                                .into_iter()
                                .filter(|it| passes_filter(&w, it.source_path.as_ref()))
                                .collect();
                            w.set_thumbnails(slint::ModelRc::new(slint::VecModel::from(filtered)));
                        } else {
                            // 4.7e — дописываем в ПОЛНУЮ; в отображаемую —
                            // только прошедшие фильтр (скролл не сбрасывается).
                            if let Some(full_model) = w
                                .get_full_thumbnails()
                                .as_any()
                                .downcast_ref::<slint::VecModel<ThumbItem>>()
                            {
                                for it in &items {
                                    full_model.push(it.clone());
                                }
                            }
                            if let Some(model) = w
                                .get_thumbnails()
                                .as_any()
                                .downcast_ref::<slint::VecModel<ThumbItem>>()
                            {
                                for it in items {
                                    if passes_filter(&w, it.source_path.as_ref()) {
                                        model.push(it);
                                    }
                                }
                            }
                        }
                        w.set_status_text(format!("{prefix} {done} (из {total})").into());
                    }
                });
                // Пауза между батчами: UI успевает отрисовать кадр.
                std::thread::sleep(std::time::Duration::from_millis(30));
            }
            log::info!("gallery: батчи миниатюр завершены ({total})");
        });
    }

    /// Запускает выбор папки + скан + генерацию миниатюр в фоновом потоке.
    /// 4.7 — после успешного скана сохраняет сессию (recent) в хранилище:
    /// при следующем входе папка откроется мгновенно, без пересканирования.
    fn scan_and_show(
        scanner: Arc<FileSystemScanner>,
        thumbs: Arc<FileSystemThumbnailProvider>,
        store: Arc<JsonSessionStore>,
        ui_weak: slint::Weak<GalleryScreen>,
        photos: Arc<Mutex<Vec<Photo>>>,
        gallery_gen: Arc<AtomicU64>,
        recursive: bool,
    ) {
        log::info!("scan_and_show: старт (recursive={recursive})");
        if let Some(w) = ui_weak.upgrade() {
            w.set_scanning(true);
            w.set_error_text("".into());
            w.set_show_start(false); // выбрали папку — уходим с экрана «Старт»
        }
        std::thread::spawn(move || {
            let folder = rfd::FileDialog::new()
                .set_title("Выберите папку с фотографиями")
                .pick_folder();

            // 4.4b — миниатюры больше НЕ строим всем списком: скан возвращает
            // только фото (метаданные), миниатюры пойдут порциями (см. ниже).
            let result: Result<(Vec<Photo>, std::path::PathBuf, String), String> = (|| {
                let folder = folder.ok_or("Папка не выбрана")?;
                log::info!("scan_and_show: выбрана папка «{}»", folder.display());
                let mode_str = if recursive { "recursive" } else { "flat" };
                let mode = if recursive {
                    ScanMode::FolderRecursive(folder.clone())
                } else {
                    ScanMode::SingleFolder(folder.clone())
                };
                let photos = scanner
                    .scan(&mode)
                    .map_err(|e| format!("Ошибка скана: {e}"))?;
                Ok((photos, folder, mode_str.to_string()))
            })();

            let ui_weak = ui_weak.clone();
            let photos_for_store = photos.clone();
            let store_for_save = store.clone();
            let thumbs_for_refresh = thumbs.clone();
            let gallery_gen_scan = gallery_gen.clone();
            let thumbs_scan = thumbs.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = ui_weak.upgrade() {
                    w.set_scanning(false);
                    match result {
                        Ok((scanned_photos, folder, mode_str)) => {
                            let count = scanned_photos.len();
                            // Сохраняем пути оригиналов для viewer.
                            *lock_or_recover(&photos_for_store) = scanned_photos.clone();
                            // 4.7 — сохраняем сессию (upsert по папке+режиму):
                            // уже существующий проект сохраняет имя и флаг.
                            let existing = store_for_save
                                .find_by_folder(&folder, &mode_str)
                                .ok()
                                .flatten();
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            let session = match &existing {
                                Some(prev) => Session {
                                    id: prev.id.clone(),
                                    name: prev.name.clone(),
                                    is_project: prev.is_project,
                                    // 4.4a — настройка листания проекта переживает
                                    // перескан (per-project, не сбрасывается).
                                    nav_mode: prev.nav_mode.clone(),
                                    fingerprint: fingerprint_of(&scanned_photos),
                                    photos: scanned_photos.clone(),
                                    last_opened: now,
                                    folder: folder.clone(),
                                    mode: mode_str.clone(),
                                },
                                None => {
                                    let name = folder
                                        .file_name()
                                        .map(|n| n.to_string_lossy().to_string())
                                        .unwrap_or_else(|| folder.to_string_lossy().to_string());
                                    Session {
                                        // id = хеш от (путь+режим) — стабилен между запусками.
                                        id: fingerprint_of(&[Photo {
                                            path: folder.clone(),
                                            file_name: name.clone(),
                                            size_bytes: 0,
                                            modified: 0,
                                        }]),
                                        name,
                                        folder: folder.clone(),
                                        mode: mode_str.clone(),
                                        fingerprint: fingerprint_of(&scanned_photos),
                                        photos: scanned_photos.clone(),
                                        last_opened: now,
                                        is_project: false,
                                        // 4.4a — новый проект: "" = edge (до края).
                                        nav_mode: String::new(),
                                    }
                                }
                            };
                            if let Err(e) = store_for_save.save(&session) {
                                log::error!("sessions: не удалось сохранить сессию: {e}");
                            }
                            // 4.7g — имя нового проекта сразу в верхнюю панель.
                            if let Some(w) = ui_weak.upgrade() {
                                w.set_current_session_id(session.id.clone().into());
                                w.set_current_project_name(session.name.clone().into());
                            }
                            refresh_session_list(&store_for_save, &thumbs_for_refresh, &ui_weak);
                            // 4.4b — порционные миниатюры: первый батч сразу,
                            // остальное батчами по 150 в фоне (см. функцию).
                            let prefix = format!("Найдено фото: {count}");
                            load_thumbs_batched(
                                scanned_photos,
                                &thumbs_scan,
                                &ui_weak,
                                &gallery_gen_scan,
                                prefix,
                            );
                        }
                        Err(e) => {
                            w.set_error_text(e.into());
                            w.set_status_text("Готово".into());
                        }
                    }
                }
            });
        });
    }

    // Колбэк «Выбрать папку» (без подпапок).
    {
        let scanner = scanner.clone();
        let thumbs = thumbs.clone();
        let store = store.clone();
        let ui_weak = ui.as_weak();
        let photos = photos.clone();
        let gallery_gen = gallery_gen.clone();
        ui.on_pick_folder_flat(move || {
            scan_and_show(
                scanner.clone(),
                thumbs.clone(),
                store.clone(),
                ui_weak.clone(),
                photos.clone(),
                gallery_gen.clone(),
                false,
            );
        });
    }
    // Колбэк «Папка с подпапками».
    {
        let scanner = scanner.clone();
        let thumbs = thumbs.clone();
        let store = store.clone();
        let ui_weak = ui.as_weak();
        let photos = photos.clone();
        let gallery_gen = gallery_gen.clone();
        ui.on_pick_folder_recursive(move || {
            scan_and_show(
                scanner.clone(),
                thumbs.clone(),
                store.clone(),
                ui_weak.clone(),
                photos.clone(),
                gallery_gen.clone(),
                true,
            );
        });
    }
    // 4.7 — открыть сессию по id (мгновенно, из хранилища).
    // Показ мгновенно из кэша; затем ФОНОМ проверяем fingerprint (4.7d):
    // совпал — ничего не делаем; изменился — инкрементальный досскан разницы.
    {
        let store = store.clone();
        let thumbs = thumbs.clone();
        let scanner = scanner.clone();
        let ui_weak = ui.as_weak();
        let photos = photos.clone();
        let viewer_ctx = viewer_ctx.clone();
        let gallery_gen = gallery_gen.clone();
        ui.on_open_session(move |id| {
            log::info!("sessions: открыть сессию id={id}");
            let session = match store.load(&id) {
                Ok(s) => s,
                Err(e) => {
                    log::error!("sessions: не удалось загрузить: {e}");
                    if let Some(w) = ui_weak.upgrade() {
                        w.set_error_text(format!("Сессия не открылась: {e}").into());
                    }
                    return;
                }
            };
            // 4.4a — МГНОВЕННЫЙ показ без фриза: в UI-потоке только дешёвые
            // операции (JSON уже прочитан, список путей склонирован). Тяжёлое —
            // миниатюры всех фото (у заказчика 10653 шт.), сохранение сессии
            // и проверка fingerprint — уходит в ОДИН фоновый поток со строгим
            // порядком: сначала миниатюры на экран, потом проверка папки.
            // Раньше thumbnails() звался синхронно в UI-потоке → окно
            // «зависало» на 10-60 секунд на больших проектах.
            let session_photos = session.photos.clone();
            *lock_or_recover(&photos) = session_photos;
            // 4.4a — режим листания проекта: в контекст вьюера и панель ⚙.
            // parse() — трейт FromStr; пустая строка/мусор → Edge (по умолчанию).
            // 4.4c — set_nav строит перемешанный список, если режим random.
            let nav: NavMode = session.nav_mode.parse().unwrap_or(NavMode::Edge);
            viewer_ctx.set_nav(nav);
            if let Some(w) = ui_weak.upgrade() {
                w.set_show_start(false);
                w.set_nav_mode(nav.as_str().into());
                // 4.7g — фиксируем открытый проект в верхней панели.
                w.set_current_session_id(session.id.clone().into());
                w.set_current_project_name(session.name.clone().into());
                w.set_status_text(format!("Открываю проект «{}»…", session.name).into());
            }
            // Обновляем last_opened (recent поднимается вверх списка).
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let mut updated = session.clone();
            updated.last_opened = now;

            // 4.4a — ФОНОВЫЙ поток: миниатюры → сохранение → fingerprint.
            let store_bg = store.clone();
            let thumbs_bg = thumbs.clone();
            let scanner_bg = scanner.clone();
            let ui_weak_bg = ui_weak.clone();
            let photos_bg = photos.clone();
            let gallery_gen_bg = gallery_gen.clone();
            let session_bg = updated.clone();
            std::thread::spawn(move || {
                // 1) 4.4b — порционные миниатюры: первый батч (200) оживляет
                // сетку сразу, остальные добавляются батчами по 150 в фоне —
                // UI-поток получает маленькие порции вместо одного куска на
                // 10k карточек (раньше «тупило» 30-60 сек даже из тёплого кэша:
                // 10k PNG декодировались + 10k карточек создавались разом).
                let prefix = format!("Проект «{}»: фото", session_bg.name);
                load_thumbs_batched(
                    session_bg.photos.clone(),
                    &thumbs_bg,
                    &ui_weak_bg,
                    &gallery_gen_bg,
                    prefix,
                );
                // 2) Сохраняем last_opened (диск I/O — в фоне, не в UI).
                if let Err(e) = store_bg.save(&session_bg) {
                    log::error!("sessions: не удалось обновить last_opened: {e}");
                }
                // Стартовый экран обновится с новым порядком recent.
                {
                    let store_r = store_bg.clone();
                    let thumbs_r = thumbs_bg.clone();
                    let ui_weak_r = ui_weak_bg.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        refresh_session_list(&store_r, &thumbs_r, &ui_weak_r);
                    });
                }
                // 3) 4.7d — ФОНОВАЯ проверка актуальности: сканируем папку
                // заново (метаданные, быстро) и сверяем fingerprint. Показ уже
                // на экране — пользователь не ждёт. Совпал → ничего;
                // изменился → досскан разницы.
                let mode = if session_bg.mode == "recursive" {
                    ScanMode::FolderRecursive(session_bg.folder.clone())
                } else {
                    ScanMode::SingleFolder(session_bg.folder.clone())
                };
                let fresh = match scanner_bg.scan(&mode) {
                    Ok(p) => p,
                    Err(e) => {
                        log::warn!("sessions: фоновая проверка — скан не удался: {e}");
                        return;
                    }
                };
                let fresh_fp = fingerprint_of(&fresh);
                if fresh_fp == session_bg.fingerprint {
                    log::info!("sessions: папка не изменилась (fingerprint совпал) — кэш актуален");
                    return;
                }
                // Папка изменилась: инкрементально — только новые/изменённые.
                // Миниатюры разницы генерируются (и пишутся в дисковый кэш)
                // вызовом thumbnails(); для показа ниже берём полный список.
                let changed = crate::session::diff_photos(&session_bg.photos, &fresh);
                log::info!(
                    "sessions: папка изменилась — досскан {} из {} фото",
                    changed.len(),
                    fresh.len()
                );
                let _ = thumbs_bg.thumbnails(&changed);
                // Обновляем сессию в хранилище (новый список + fingerprint).
                let mut updated_session = session_bg.clone();
                updated_session.photos = fresh.clone();
                updated_session.fingerprint = fresh_fp;
                if let Err(e) = store_bg.save(&updated_session) {
                    log::error!("sessions: не удалось обновить сессию: {e}");
                }
                // Показываем ПОЛНЫЙ новый список — порционно (4.4b): первый
                // батч заменит модель, остальное добавится в фоне. Список
                // путей для viewer обновляем сразу (метаданные уже в памяти).
                *lock_or_recover(&photos_bg) = fresh.clone();
                let prefix = format!("Проект «{}»: обновлено, фото", updated_session.name);
                load_thumbs_batched(fresh, &thumbs_bg, &ui_weak_bg, &gallery_gen_bg, prefix);
            });
        });
    }
    // 4.7 — закрыть экран «Старт».
    {
        let ui_weak = ui.as_weak();
        ui.on_close_start(move || {
            if let Some(w) = ui_weak.upgrade() {
                w.set_show_start(false);
            }
        });
    }
    // 4.7e — переименовать проект: открываем Slint-диалог (id запоминаем).
    {
        let store = store.clone();
        let ui_weak = ui.as_weak();
        ui.on_rename_session(move |id| {
            // Текущее имя берём из хранилища (надёжнее, чем из UI-модели).
            let current_name = store.load(&id).map(|s| s.name).unwrap_or_default();
            if let Some(w) = ui_weak.upgrade() {
                w.set_rename_text(current_name.into());
                w.set_rename_id(id);
                w.set_rename_visible(true);
            }
        });
    }
    // 4.7e — подтвердить переименование: имя из диалога, сессия = проект.
    {
        let store = store.clone();
        let thumbs = thumbs.clone();
        let ui_weak = ui.as_weak();
        ui.on_rename_confirm(move || {
            let (id, name) = {
                let Some(w) = ui_weak.upgrade() else { return };
                (w.get_rename_id(), w.get_rename_text())
            };
            if name.trim().is_empty() {
                return; // пустое имя не сохраняем
            }
            let mut session = match store.load(&id) {
                Ok(s) => s,
                Err(e) => {
                    log::error!("sessions: rename — не найдена: {e}");
                    return;
                }
            };
            session.name = name.trim().to_string();
            session.is_project = true; // переименование = стал именованным проектом
            if let Err(e) = store.save(&session) {
                log::error!("sessions: rename — не удалось сохранить: {e}");
            }
            if let Some(w) = ui_weak.upgrade() {
                w.set_rename_visible(false);
                // 4.7g — если переименовали ОТКРЫТЫЙ проект, обновляем панель.
                if w.get_current_session_id() == id {
                    w.set_current_project_name(session.name.clone().into());
                }
            }
            refresh_session_list(&store, &thumbs, &ui_weak);
        });
    }
    // 4.7e — отменить переименование.
    {
        let ui_weak = ui.as_weak();
        ui.on_rename_cancel(move || {
            if let Some(w) = ui_weak.upgrade() {
                w.set_rename_visible(false);
            }
        });
    }
    // 4.7e — удалить сессию.
    {
        let store = store.clone();
        let thumbs = thumbs.clone();
        let ui_weak = ui.as_weak();
        ui.on_delete_session(move |id| {
            if let Err(e) = store.delete(&id) {
                log::error!("sessions: delete — не удалось: {e}");
            }
            refresh_session_list(&store, &thumbs, &ui_weak);
        });
    }
    // 4.7g — «← Проекты»: вернуться на стартовый экран из открытого проекта.
    {
        let ui_weak = ui.as_weak();
        ui.on_show_projects(move || {
            if let Some(w) = ui_weak.upgrade() {
                w.set_show_start(true);
            }
        });
    }
    // 4.7e-fix3 — «🔄 Пересканировать»: свежий скан папки проекта с диска.
    // Кэш сессии мог быть записан СТАРОЙ версией (например, до поддержки
    // видео) — кнопка честно обновляет список и кэш, без удаления сессии.
    {
        let store = store.clone();
        let scanner = scanner.clone();
        let thumbs = thumbs.clone();
        let ui_weak = ui.as_weak();
        let photos = photos.clone();
        let gallery_gen = gallery_gen.clone();
        ui.on_rescan_project(move || {
            // id открытого проекта; пустой = галерея из диалога, не сессия.
            let id: String = ui_weak
                .upgrade()
                .map(|w| w.get_current_session_id().to_string())
                .unwrap_or_default();
            if id.is_empty() {
                log::warn!("rescan: проект не открыт — нечего пересканировать");
                return;
            }
            let session = match store.load(&id) {
                Ok(s) => s,
                Err(e) => {
                    log::error!("rescan: сессия не читается: {e}");
                    return;
                }
            };
            log::info!(
                "rescan: пересканирую «{}» ({})",
                session.name,
                session.folder.display()
            );
            if let Some(w) = ui_weak.upgrade() {
                w.set_status_text("Пересканирую папку…".into());
            }
            let store_bg = store.clone();
            let scanner_bg = scanner.clone();
            let thumbs_bg = thumbs.clone();
            let ui_weak_bg = ui_weak.clone();
            let photos_bg = photos.clone();
            let gallery_gen_bg = gallery_gen.clone();
            std::thread::spawn(move || {
                // Свежий скан в режиме сессии.
                let mode = if session.mode == "recursive" {
                    ScanMode::FolderRecursive(session.folder.clone())
                } else {
                    ScanMode::SingleFolder(session.folder.clone())
                };
                let fresh = match scanner_bg.scan(&mode) {
                    Ok(p) => p,
                    Err(e) => {
                        log::error!("rescan: скан не удался: {e}");
                        if let Some(w) = ui_weak_bg.upgrade() {
                            w.set_status_text("Пересканирование не удалось".into());
                        }
                        return;
                    }
                };
                let fresh_fp = fingerprint_of(&fresh);
                log::info!(
                    "rescan: найдено {} файлов (было {})",
                    fresh.len(),
                    session.photos.len()
                );
                // Обновляем сессию: новый список + fingerprint.
                let mut updated = session.clone();
                updated.photos = fresh.clone();
                updated.fingerprint = fresh_fp;
                if let Err(e) = store_bg.save(&updated) {
                    log::error!("rescan: не удалось сохранить сессию: {e}");
                }
                // Показ: список для viewer сразу, миниатюры порционно.
                *lock_or_recover(&photos_bg) = fresh.clone();
                let _ = slint::invoke_from_event_loop({
                    let ui_weak_bg = ui_weak_bg.clone();
                    let name = updated.name.clone();
                    move || {
                        if let Some(w) = ui_weak_bg.upgrade() {
                            w.set_status_text(
                                format!("Проект «{name}»: пересканировано, фото").into(),
                            );
                        }
                    }
                });
                let prefix = format!("Проект «{}»: пересканировано, фото", updated.name);
                load_thumbs_batched(fresh, &thumbs_bg, &ui_weak_bg, &gallery_gen_bg, prefix);
            });
        });
    }
    // 4.7g — открыть панель настроек проекта (каркас под 4.4).
    {
        let ui_weak = ui.as_weak();
        ui.on_open_settings(move || {
            if let Some(w) = ui_weak.upgrade() {
                w.set_settings_visible(true);
            }
        });
    }
    // 4.7g — закрыть панель настроек.
    {
        let ui_weak = ui.as_weak();
        ui.on_close_settings(move || {
            if let Some(w) = ui_weak.upgrade() {
                w.set_settings_visible(false);
            }
        });
    }
    // 4.7e — галочка фильтра сменилась: пересобрать отображаемую модель
    // из полной по активным расширениям (мгновенно, миниатюры не трогаем).
    {
        let ui_weak = ui.as_weak();
        ui.on_filter_changed(move || {
            if let Some(w) = ui_weak.upgrade() {
                rebuild_filtered_thumbnails(&w);
            }
        });
    }
    // 4.7e — «Все»/«Ничего»: выставить все галочки и пересобрать модель.
    {
        let ui_weak = ui.as_weak();
        ui.on_filter_set_all(move |on| {
            if let Some(w) = ui_weak.upgrade() {
                w.set_filter_jpg(on);
                w.set_filter_png(on);
                w.set_filter_gif(on);
                w.set_filter_bmp(on);
                w.set_filter_webp(on);
                w.set_filter_mp4(on);
                w.set_filter_webm(on);
                w.set_filter_mov(on);
                w.set_filter_mkv(on);
                w.set_filter_avi(on);
                rebuild_filtered_thumbnails(&w);
            }
        });
    }
    // 4.4a — сменить режим листания проекта (edge/cycle/random) из панели ⚙.
    // Обновляем: контекст вьюера (листание сразу в новом режиме), property UI
    // (подсветка переключателя) и сессию в хранилище (переживает перезапуск).
    {
        let store = store.clone();
        let ui_weak = ui.as_weak();
        let viewer_ctx = viewer_ctx.clone();
        ui.on_set_nav_mode(move |mode| {
            let nav: NavMode = mode.parse().unwrap_or(NavMode::Edge);
            log::info!("settings: режим листания → {}", nav.as_str());
            // 4.4c — set_nav строит перемешанный список при включении random
            // (и сбрасывает при выключении).
            viewer_ctx.set_nav(nav);
            if let Some(w) = ui_weak.upgrade() {
                w.set_nav_mode(nav.as_str().into());
            }
            // Сохраняем в сессию, если открыт проект (id непустой).
            let id: String = ui_weak
                .upgrade()
                .map(|w| w.get_current_session_id().to_string())
                .unwrap_or_default();
            if id.is_empty() {
                return; // галерея открыта из диалога, не сессия — сохранять некуда
            }
            match store.load(&id) {
                Ok(mut s) => {
                    s.nav_mode = nav.as_str().to_string();
                    if let Err(e) = store.save(&s) {
                        log::error!("settings: не удалось сохранить nav_mode: {e}");
                    }
                }
                Err(e) => log::error!("settings: сессия {id} не читается: {e}"),
            }
        });
    }
    // 4.6c — инициализация панели настроек из глобальных настроек:
    // галка + бегунок ядер + максимум бегунка; 4.6c-fix3 — вкладка
    // по умолчанию (0 = «Листание») и галка «Детальная статистика».
    {
        let ui_weak = ui.as_weak();
        let (on, threads) = global_settings.snapshot();
        let max_threads = std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(1);
        if let Some(w) = ui_weak.upgrade() {
            w.set_ultra_speed(on);
            w.set_ultra_threads(threads as i32);
            w.set_ultra_threads_max(max_threads);
            w.set_settings_tab(0);
            // 4.8 — уровень логов из настроек (заменил галку «Детальная
            // статистика»); применяем к логгеру сразу.
            let log_level = global_settings.log_level.load(Ordering::Relaxed);
            crate::logging::set_level(log_level);
            w.set_log_level(log_level as i32);
            // 4.7a — «Зациклить анимацию» из глобальных настроек.
            w.set_loop_animation(global_settings.loop_animation_on());
        }
    }
    // 4.6c — галка «Ультра-скорость»: пишем в атомики (воркер применит к
    // СЛЕДУЮЩЕМУ кадру) и сохраняем на диск (переживает перезапуск).
    {
        let ui_weak = ui.as_weak();
        let settings = global_settings.clone();
        ui.on_set_ultra_speed(move |on| {
            log::info!("settings: Ультра-скорость → {on}");
            settings.ultra_speed.store(on, Ordering::Relaxed);
            crate::settings::store(&settings);
            if let Some(w) = ui_weak.upgrade() {
                w.set_ultra_speed(on);
            }
        });
    }
    // 4.6c — бегунок числа ядер: клампим 1..max, пишем в атомик,
    // сохраняем на диск. UI-подпись обновится сама через property.
    {
        let ui_weak = ui.as_weak();
        let settings = global_settings.clone();
        ui.on_set_ultra_threads(move |n| {
            let max = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1);
            let n = n.clamp(1, max as i32) as usize;
            log::info!("settings: ядер для Ультра-скорости → {n}");
            settings.threads.store(n, Ordering::Relaxed);
            crate::settings::store(&settings);
            if let Some(w) = ui_weak.upgrade() {
                w.set_ultra_threads(n as i32);
            }
        });
    }
    // 4.6c-fix3 — переключить вкладку настроек (0/1/2): чисто UI-дело,
    // клампим в диапазон и пишем property (хранилище не трогаем —
    // выбранная вкладка не должна переживать перезапуск, KISS).
    {
        let ui_weak = ui.as_weak();
        ui.on_set_settings_tab(move |tab| {
            let tab = tab.clamp(0, 2);
            if let Some(w) = ui_weak.upgrade() {
                w.set_settings_tab(tab);
            }
        });
    }
    // 4.8 — уровень подробности лога (3 таба вкладки «Отладка»):
    // пишем в атомик, применяем к логгеру СРАЗУ (set_level), сохраняем.
    {
        let ui_weak = ui.as_weak();
        let settings = global_settings.clone();
        ui.on_set_log_level(move |level| {
            let level = level.clamp(0, 2) as u8;
            log::info!("settings: уровень логов → {level}");
            settings.log_level.store(level, Ordering::Relaxed);
            crate::logging::set_level(level);
            crate::settings::store(&settings);
            if let Some(w) = ui_weak.upgrade() {
                w.set_log_level(level as i32);
            }
        });
    }
    // 4.7a/4.7d — зацикливание: тумблер 🔁 на панели анимации (переехал
    // из настроек). Пишем в атомик (аниматор читает на каждом проходе —
    // смена применяется к текущей анимации сразу) и сохраняем на диск.
    {
        let ui_weak = ui.as_weak();
        let settings = global_settings.clone();
        ui.on_set_loop_animation(move |on| {
            log::info!("anim: зацикливание → {on}");
            settings.loop_animation.store(on, Ordering::Relaxed);
            crate::settings::store(&settings);
            if let Some(w) = ui_weak.upgrade() {
                w.set_loop_animation(on);
            }
        });
    }
    // 4.7d — бегунок перемотки: кадр 0..total-1 → запрос аниматору.
    // Сверка с текущим кадром подавляет петлю «аниматор двигает слайдер
    // → changed → seek того же кадра» (безопасно, но лишний invoke).
    {
        let ctx = viewer_ctx.clone();
        ui.on_anim_seek(move |frame| {
            let frame = frame.max(0);
            if ctx.playback.current_frame() as i32 == frame {
                return; // слайдер уже на этом кадре (его двигает аниматор)
            }
            log::debug!("anim: перемотка → кадр {frame}");
            ctx.playback.request_seek(frame);
        });
    }
    // 4.7d — кнопка ⏸/▶: переключаем паузу и её отражение в UI.
    // 4.7d-fix — ▶ на АВТОПАУЗЕ конца прохода: перезапуск с кадра 0
    // (иначе «продолжить» с последнего кадра мгновенно упиралось в
    // конец и снова паузило — кнопка «не работала»).
    {
        let ctx = viewer_ctx.clone();
        ui.on_anim_toggle_pause(move || {
            let paused = !ctx.playback.is_paused();
            log::info!("anim: пауза → {paused}");
            // Снимаем паузу, стоя на последнем кадре — честный
            // перезапуск с начала.
            if !paused {
                let total = ctx.playback.total();
                if total > 0 && ctx.playback.current_frame() + 1 >= total {
                    ctx.playback.request_seek(0);
                }
            }
            ctx.playback.set_paused(paused);
            if let Some(w) = ctx.ui_weak.upgrade() {
                w.set_anim_paused(paused);
            }
        });
    }
    // 4.7d — бегунок скорости: десятые множителя 1..20 (×0.1..×2.0).
    // Аниматор читает на каждом кадре — применяется мгновенно.
    {
        let ctx = viewer_ctx.clone();
        ui.on_anim_set_speed(move |tenths| {
            let tenths = tenths.clamp(1, 20);
            log::info!("anim: скорость → ×{}.{}", tenths / 10, tenths % 10);
            ctx.playback.set_speed_tenths(tenths as u32);
            if let Some(w) = ctx.ui_weak.upgrade() {
                w.set_anim_speed(tenths);
            }
        });
    }
    // 4.6c-fix4 — открыть папку с логами в Проводнике (Windows).
    // Папку берём из пути лога (logging::log_path), не дублируем знание.
    {
        ui.on_open_logs_folder(move || {
            let dir = crate::logging::log_path()
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            log::info!("debug: открыть папку логов: {}", dir.display());
            match std::process::Command::new("explorer").arg(&dir).spawn() {
                Ok(_) => {}
                Err(e) => log::error!("debug: не открыть папку логов: {e}"),
            }
        });
    }
    // 4.6c-fix4 — открыть сам лог в программе по умолчанию (блокнот).
    // «cmd /c start "" путь» — пустой заголовок обязателен: без него
    // start трактует путь в кавычках как заголовок окна.
    {
        ui.on_open_log_file(move || {
            let path = crate::logging::log_path();
            log::info!("debug: открыть лог: {}", path.display());
            match std::process::Command::new("cmd")
                .args(["/c", "start", "", &path.display().to_string()])
                .spawn()
            {
                Ok(_) => {}
                Err(e) => log::error!("debug: не открыть лог: {e}"),
            }
        });
    }
    // 4.6c-fix4 — сохранить копию лога: нативный диалог «куда» (rfd,
    // как у выбора папки проектов) в ОТДЕЛЬНОМ потоке (диалог блокирующий,
    // UI не вешаем), затем копируем файл. Ошибки логируем, не паникуем.
    {
        ui.on_save_log_copy(move || {
            std::thread::spawn(move || {
                let src = crate::logging::log_path();
                let dst = rfd::FileDialog::new()
                    .set_title("Куда сохранить копию лога")
                    .set_file_name("photo_explorer.log")
                    .save_file();
                let Some(dst) = dst else {
                    return; // отменили диалог — ничего не делаем
                };
                match std::fs::copy(&src, &dst) {
                    Ok(bytes) => {
                        log::info!(
                            "debug: копия лога сохранена ({} байт) → {}",
                            bytes,
                            dst.display()
                        )
                    }
                    Err(e) => log::error!("debug: не сохранить копию лога: {e}"),
                }
            });
        });
    }

    // --- Viewer колбэги ---
    // Открыть фото по индексу.
    // ВАЖНО: именно здесь включаем ПОЛНОЭКРАННЫЙ режим (как в играх) —
    // окно разворачивается на текущий монитор. Само приложение в обычном
    // оконном режиме; на весь экран уходит только просмотр фото.
    {
        let ctx = viewer_ctx.clone();
        ui.on_open_viewer(move |model_index| {
            // 4.7a-fix — КОРЕНЬ рассинхрона «клик по миниатюре открывает
            // другое фото»: index из клика — это позиция в МОДЕЛИ
            // thumbnails, а photos — ДРУГОЙ список (расходятся при
            // батчах/досскане, урок 4.4c-6). Решение то же, что в
            // show_thumbnail: истина — ПУТЬ. Берём source_path из модели
            // и ищем индекс В PHOTOS по пути. Миниатюра-сирота (пути нет
            // в photos) — не открываем ничего.
            use slint::Model;
            let path_str: String = {
                let Some(w) = ctx.ui_weak.upgrade() else {
                    return;
                };
                let thumbs = w.get_thumbnails();
                let Some(item) = thumbs.row_data(model_index as usize) else {
                    log::warn!("viewer: клик по миниатюре {model_index} — нет в модели");
                    return;
                };
                item.source_path.to_string()
            };
            let index: i32 = {
                let p = lock_or_recover(&ctx.photos);
                match p
                    .iter()
                    .position(|ph| ph.path.to_string_lossy() == path_str)
                {
                    Some(i) => i as i32,
                    None => {
                        log::warn!(
                            "viewer: миниатюра «{path_str}» не найдена в photos — не открываем"
                        );
                        return;
                    }
                }
            };
            if index != model_index {
                log::debug!(
                    "[FIX] клик: модель={model_index}, photos={index} — рассинхрон, открыли по пути"
                );
            }
            log::info!("viewer: открыть фото index={index}");
            ctx.last_direction.store(1, Ordering::SeqCst); // 4.3e — открытие = «вперёд».
                                                           // 4.4c-2 — открытие кликом в random: синхронизируем shuffle_pos с
                                                           // открытым фото (иначе next/prev считали бы от старой позиции).
            if *ctx.nav_mode.borrow() == NavMode::Random {
                let order = ctx.shuffle_order.borrow();
                if let Some(order) = order.as_ref() {
                    if let Some(p) = order.iter().position(|&i| i == index) {
                        ctx.shuffle_pos.set(p as i32);
                    }
                }
            }
            if let Some(w) = ctx.ui_weak.upgrade() {
                w.set_viewer_index(index);
                w.set_show_viewer(true);
                // Сброс зума при открытии нового фото (иначе откроется зумленным).
                w.set_viewer_zoom(1.0);
                w.set_viewer_offset_x(0.0);
                w.set_viewer_offset_y(0.0);
                // Полноэкранный режим на активном мониторе (где сейчас окно).
                w.window().set_fullscreen(true);
                log::info!("viewer: fullscreen ON");
            }
            // 4.3i — единый переход: одно поколение делят миниатюра (мгновенно
            // в единый viewer-image) и чёткое фото (перезапишет тот же слой).
            ctx.switch_to_photo(index, 0, true);
        });
    }
    // Закрыть viewer: выходим из полноэкранного режима, возвращаем оконный.
    {
        let ui_weak = ui.as_weak();
        ui.on_close_viewer(move || {
            log::info!("viewer: закрытие");
            if let Some(w) = ui_weak.upgrade() {
                w.set_show_viewer(false);
                w.window().set_fullscreen(false);
                log::info!("viewer: fullscreen OFF");
            }
        });
    }
    // Следующее/предыдущее фото — единая схема 4.4a: индекс считает ЧИСТАЯ
    // функция nav_next по режиму проекта (edge — стоп на краю / cycle —
    // заворот / random — случайный кадр; обе кнопки случайные — решение
    // заказчика). None = стоп на границе: остаёмся, ничего не грузим.
    // Без debounce (он затормаживал листание): грузим сразу.
    // 4.3i — слой один: миниатюра нового кадра сама перезаписывает viewer-image,
    // промежуточная очистка не нужна (она давала бы чёрный кадр-вспышку).
    {
        let ctx = viewer_ctx.clone();
        ui.on_viewer_next(move || {
            ctx.last_direction.store(1, Ordering::SeqCst); // 4.3e — листаем вперёд.
            let count = lock_or_recover(&ctx.photos).len() as i32;
            let cur = ctx
                .ui_weak
                .upgrade()
                .map(|w| w.get_viewer_index())
                .unwrap_or(0);
            let mode = *ctx.nav_mode.borrow();
            // 4.4c — random идёт по перемешанному списку: next = следующая
            // ПОЗИЦИЯ в нём (порядок фиксирован — «назад» будет честным).
            let idx = match mode {
                NavMode::Random => {
                    let order = ctx.shuffle_order.borrow();
                    let Some(order) = order.as_ref() else {
                        return; // список не построен (сессия пуста) — выходим
                    };
                    let broken = ctx.broken.borrow();
                    let Some((np, photo)) =
                        nav_random_next(order, ctx.shuffle_pos.get(), 1, &broken)
                    else {
                        return; // край перемешанного списка — стоп
                    };
                    drop(broken);
                    ctx.shuffle_pos.set(np);
                    photo
                }
                _ => {
                    let broken = ctx.broken.borrow();
                    let Some(idx) = nav_next(cur, count, 1, mode, &broken) else {
                        return; // edge: дошли до края — стоп, ничего не грузим
                    };
                    idx
                }
            };
            if let Some(w) = ctx.ui_weak.upgrade() {
                log::debug!("viewer: next → index={idx} (mode={})", mode.as_str());
                w.set_viewer_index(idx);
                w.set_viewer_zoom(1.0); // сброс зума для нового фото
                w.set_viewer_offset_x(0.0);
                w.set_viewer_offset_y(0.0);
            }
            // 4.3i — единый переход: одно поколение делят миниатюра и чёткое.
            ctx.switch_to_photo(idx, 0, true);
        });
    }
    // Предыдущее фото — та же схема (dir = −1); в random — предыдущая
    // позиция перемешанного списка (честный возврат просмотренного).
    {
        let ctx = viewer_ctx.clone();
        ui.on_viewer_prev(move || {
            ctx.last_direction.store(-1, Ordering::SeqCst); // 4.3e — листаем назад.
            let count = lock_or_recover(&ctx.photos).len() as i32;
            let cur = ctx
                .ui_weak
                .upgrade()
                .map(|w| w.get_viewer_index())
                .unwrap_or(0);
            let mode = *ctx.nav_mode.borrow();
            let idx = match mode {
                NavMode::Random => {
                    let order = ctx.shuffle_order.borrow();
                    let Some(order) = order.as_ref() else {
                        return;
                    };
                    let broken = ctx.broken.borrow();
                    let Some((np, photo)) =
                        nav_random_next(order, ctx.shuffle_pos.get(), -1, &broken)
                    else {
                        return; // начало перемешанного списка — стоп
                    };
                    drop(broken);
                    ctx.shuffle_pos.set(np);
                    photo
                }
                _ => {
                    let broken = ctx.broken.borrow();
                    let Some(idx) = nav_next(cur, count, -1, mode, &broken) else {
                        return; // edge: дошли до первого фото — стоп (фикс «в минус»)
                    };
                    idx
                }
            };
            if let Some(w) = ctx.ui_weak.upgrade() {
                log::debug!("viewer: prev → index={idx} (mode={})", mode.as_str());
                w.set_viewer_index(idx);
                w.set_viewer_zoom(1.0); // сброс зума для нового фото
                w.set_viewer_offset_x(0.0);
                w.set_viewer_offset_y(0.0);
            }
            // 4.3i — единый переход: одно поколение делят миниатюра и чёткое.
            ctx.switch_to_photo(idx, 0, true);
        });
    }

    // 4.3e — авто-скип битого файла: воркер через request-skip просит показать
    // фото next-index в направлении dir. Защита от зацикливания: если
    // skip_depth превысил размер списка — все файлы битые, показываем плашку.
    // next-index и dir приходят из воркера; skip_depth — сколько уже скипнули.
    {
        let ctx = viewer_ctx.clone();
        // 4.4c-2 — в random dir ВАЖЕН: это направление шага по shuffle-списку
        // (воркер его не знает, но last_direction хранит последнее листание).
        // В edge/cycle dir по-прежнему не нужен (воркер прислал готовый
        // линейный next_index).
        // 4.3h — my_gen: поколение, при котором воркер нашёл битый файл.
        ui.on_request_skip(move |next_index, dir, skip_depth, my_gen| {
            // 4.3h — ГЛАВНЫЙ фикс: скип, как и обычная загрузка, сверяет
            // поколение. Если пользователь уже листнул дальше (generation ушёл
            // вперёд), устаревший скип игнорируется — он больше не накладывает
            // свой кадр поверх актуального («в фоне один, поверх другой»).
            if ctx.generation.load(Ordering::SeqCst) != my_gen as u64 {
                log::debug!(
                    "request-skip: устарел my={my_gen} cur={}",
                    ctx.generation.load(Ordering::SeqCst)
                );
                return;
            }
            let count = lock_or_recover(&ctx.photos).len() as i32;
            // Защита от зацикливания: перепробовали больше, чем файлов в списке.
            if skip_depth > count || count == 0 {
                log::error!(
                    "request-skip: все файлы битые (depth={skip_depth}, count={count}) — плашка"
                );
                if let Some(w) = ctx.ui_weak.upgrade() {
                    w.set_viewer_image(slint::Image::default());
                    w.set_viewer_error("Файл не открывается".into());
                }
                return;
            }
            // 4.4c-3 — помечаем битый индекс (пришёл от воркера как next_index
            // минус dir = сам битый): навигация всех режимов его пропустит.
            let broken_idx = next_index - dir;
            if broken_idx >= 0 && broken_idx < count {
                ctx.broken.borrow_mut().insert(broken_idx);
            }
            // 4.4c-2 — в random скип идёт ПО ПЕРЕМЕШАННОМУ СПИСКУ, а не к
            // линейному соседу: иначе случайный кадр «убегал» на линейного
            // соседа (в логе: random дал 1531 → битый → скип линейно прыгал
            // 1532→1533→1534, кадр «сам сменялся», shuffle_pos рассинхронился).
            // Берём следующую позицию порядка от текущей shuffle_pos в
            // направлении dir; shuffle_pos синхронно обновляем.
            let mode = *ctx.nav_mode.borrow();
            let idx = if mode == NavMode::Random {
                let order = ctx.shuffle_order.borrow();
                let Some(order) = order.as_ref() else {
                    // Список не построен (пустая сессия) — плашка.
                    if let Some(w) = ctx.ui_weak.upgrade() {
                        w.set_viewer_image(slint::Image::default());
                        w.set_viewer_error("Файл не открывается".into());
                    }
                    return;
                };
                let dir_list = if dir == 0 { 1 } else { dir };
                let broken = ctx.broken.borrow();
                match nav_random_next(order, ctx.shuffle_pos.get(), dir_list, &broken) {
                    Some((np, photo)) => {
                        ctx.shuffle_pos.set(np); // синхронизация с показанным
                        photo
                    }
                    None => {
                        // Край перемешанного списка — плашка (как edge).
                        log::error!(
                            "request-skip: край shuffle-списка (pos={}, dir={dir_list}) — плашка",
                            ctx.shuffle_pos.get()
                        );
                        if let Some(w) = ctx.ui_weak.upgrade() {
                            w.set_viewer_image(slint::Image::default());
                            w.set_viewer_error("Файл не открывается".into());
                        }
                        return;
                    }
                }
            } else {
                // 4.3g — фикс «скипа через один»: воркер уже прислал ЦЕЛЕВОЙ
                // next_index (= битый index + dir), повторно dir НЕ прибавляем.
                // 4.4c-3 — если ЦЕЛЕВОЙ тоже битый (уже в broken) — шагаем
                // дальше по направлению, пока не найдём годное (без загрузки).
                let broken = ctx.broken.borrow();
                let mut idx = next_index;
                loop {
                    if idx < 0 || idx >= count {
                        // Достигли края и дальше годных нет — плашка.
                        log::error!(
                            "request-skip: край списка (idx={idx}, count={count}) — плашка"
                        );
                        if let Some(w) = ctx.ui_weak.upgrade() {
                            w.set_viewer_image(slint::Image::default());
                            w.set_viewer_error("Файл не открывается".into());
                        }
                        return;
                    }
                    if !broken.contains(&idx) {
                        break; // годное — сюда
                    }
                    idx += dir; // битое — дальше по направлению
                }
                idx
            };
            log::debug!(
                "request-skip: скип → index={idx} (depth={skip_depth}, mode={})",
                mode.as_str()
            );
            if let Some(w) = ctx.ui_weak.upgrade() {
                w.set_viewer_index(idx);
                w.set_viewer_zoom(1.0);
                w.set_viewer_offset_x(0.0);
                w.set_viewer_offset_y(0.0);
            }
            // 4.3i — переход скипа устроен как у next/prev: единый
            // switch_to_photo (одно поколение делят миниатюра и чёткое).
            // skip_depth уходит в задачу воркера: если и этот файл битый,
            // воркер снова попросит скип.
            // 4.4c-3 — show_thumb=false: НЕ показываем миниатюру кандидата
            // скипа — он ещё не проверен (если битый, миниатюра не мелькнёт).
            ctx.switch_to_photo(idx, skip_depth as u32, false);
        });
    }

    // 4.3e — автоскрытие управления: по движению мыши показываем кнопки
    // и перезапускаем таймер на 2.5с; по истечении — гасим.
    {
        let ui_weak = ui.as_weak();
        let timer = controls_timer.clone();
        ui.on_mouse_moved(move || {
            if let Some(w) = ui_weak.upgrade() {
                w.set_controls_visible(true);
                let ui_weak = ui_weak.clone();
                timer.start(
                    slint::TimerMode::SingleShot,
                    std::time::Duration::from_millis(CONTROLS_HIDE_MS),
                    move || {
                        if let Some(w) = ui_weak.upgrade() {
                            w.set_controls_visible(false);
                        }
                    },
                );
            }
        });
    }

    // 4.3b — зум к курсору: пересчёт масштаба и смещения.
    // mx,my — позиция курсора относительно центра viewport (px). up — приблизить/отдалить.
    // Формула zoom-to-point: offset' = cursor - (cursor - offset) * (zoom'/zoom),
    // чтобы точка изображения под курсором осталась под курсором.
    // 4.3f — добавлен кламп offset «край в край»: фото нельзя утащить/зазумить
    // за пределы экрана (упирается краем в край viewport, как в классических
    // просмотрщиках). Для этого slint передаёт размеры viewport (vw, vh).
    {
        let ui_weak = ui.as_weak();
        const ZOOM_STEP: f32 = 1.2; // шаг масштаба на одно деление колеса
        const ZOOM_MIN: f32 = 1.0; // не меньше вписанного
        const ZOOM_MAX: f32 = 10.0; // предел приближения
        ui.on_viewer_zoom_at(move |mx, my, up, vw, vh, iw, ih| {
            if let Some(w) = ui_weak.upgrade() {
                let zoom = w.get_viewer_zoom();
                let new_zoom = if up {
                    (zoom * ZOOM_STEP).min(ZOOM_MAX)
                } else {
                    (zoom / ZOOM_STEP).max(ZOOM_MIN)
                };
                if (new_zoom - zoom).abs() < 1e-6 {
                    return; // упёрлись в предел — ничего не делаем
                }
                // offset' = cursor - (cursor - offset) * (new_zoom / zoom)
                let ox = w.get_viewer_offset_x();
                let oy = w.get_viewer_offset_y();
                let k = new_zoom / zoom;
                let new_ox = mx - (mx - ox) * k;
                let new_oy = my - (my - oy) * k;
                // 4.3g — кламп «край в край» ПО КАРТИНКЕ (contain-fit):
                // у узких/широких фото картинка меньше элемента vp*zoom.
                let new_ox = clamp_offset(new_ox, new_zoom, vw, iw, ih, vh);
                let new_oy = clamp_offset(new_oy, new_zoom, vh, ih, iw, vw);
                // При возврате к zoom=1 центрируем (offset=0), чтобы фото встало ровно.
                if (new_zoom - 1.0).abs() < 1e-6 {
                    w.set_viewer_offset_x(0.0);
                    w.set_viewer_offset_y(0.0);
                } else {
                    w.set_viewer_offset_x(new_ox);
                    w.set_viewer_offset_y(new_oy);
                }
                w.set_viewer_zoom(new_zoom);
                log::debug!(
                    "viewer: zoom {zoom:.2}→{new_zoom:.2}, offset ({new_ox:.0},{new_oy:.0})"
                );
            }
        });
    }
    // 4.3f — инкрементальный pan: добавить к offset дельту мыши (dx, dy) с клампом
    // «край в край» по размерам viewport (vw, vh). Вызывается из slint на каждом
    // pointer-move с зажатой ЛКМ (zoom>1). Инкрементальность убирает конфликт
    // с зумом: pan не опирается на замороженный стартовый offset.
    {
        let ui_weak = ui.as_weak();
        ui.on_viewer_pan_by(move |dx, dy, vw, vh, iw, ih| {
            if let Some(w) = ui_weak.upgrade() {
                let zoom = w.get_viewer_zoom();
                // 4.3g — кламп по КАРТИНКЕ (contain-fit), не по элементу.
                let new_ox = clamp_offset(w.get_viewer_offset_x() + dx, zoom, vw, iw, ih, vh);
                let new_oy = clamp_offset(w.get_viewer_offset_y() + dy, zoom, vh, ih, iw, vw);
                w.set_viewer_offset_x(new_ox);
                w.set_viewer_offset_y(new_oy);
            }
        });
    }

    ui.run()
}
