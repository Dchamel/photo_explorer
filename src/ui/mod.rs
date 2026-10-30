//! Слой интерфейса на Slint.
//!
//! Экран блокировки (одно окно): точки PIN + встроенная клавиатура + автовход.
//! Биометрия запускается в ОТДЕЛЬНОМ потоке (op.get() блокирующий) и результат
//! возвращается в UI-поток через slint::invoke_from_event_loop. Флаг «идёт» защищает
//! от повторных запросов (иначе дедлок/зависание).

slint::include_modules!();

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
/// Random-режим обрабатывает nav_random_next (шаг по перемешанному списку).
pub fn nav_next(cur: i32, count: i32, dir: i32, mode: NavMode) -> Option<i32> {
    if count <= 0 {
        return None; // пустой список — листать нечего
    }
    match mode {
        NavMode::Edge => {
            let ni = cur + dir;
            if ni < 0 || ni >= count {
                None // край — стоп, не переходим
            } else {
                Some(ni)
            }
        }
        NavMode::Cycle => {
            let mut ni = cur + dir;
            if ni >= count {
                ni = 0;
            } else if ni < 0 {
                ni = count - 1;
            }
            Some(ni)
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
/// Чистая функция — легко тестировать.
pub fn nav_random_next(order: &[i32], pos: i32, dir: i32) -> Option<(i32, i32)> {
    let len = order.len() as i32;
    if len <= 0 {
        return None; // пустой список — листать нечего
    }
    let np = pos + dir;
    if np < 0 || np >= len {
        return None; // край перемешанного списка — стоп
    }
    order.get(np as usize).map(|&photo| (np, photo))
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
    let viewer_cache: Arc<Mutex<ViewerCache>> =
        Arc::new(Mutex::new(ViewerCache::new(20.try_into().unwrap())));

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
    const IMAGE_CACHE_MAX: usize = 20;

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
    let (load_tx, load_rx) = mpsc::channel::<LoadTask>();
    // ui_weak для воркера (Send): Weak<GalleryScreen> — Send.
    let ui_weak_for_viewer = ui.as_weak();
    // Воркер-поток: цикл приёма задач. Send-данные (Vec<u8>) отдаём в UI
    // через invoke_from_event_loop (slint::Image — не Send, строим в UI-потоке).
    {
        let viewer_cache = viewer_cache.clone();
        let viewer_generation = viewer_generation.clone();
        let last_direction = last_direction.clone();
        let ui_weak_for_viewer = ui_weak_for_viewer.clone();
        std::thread::spawn(move || {
            log::info!("viewer worker: запущен (1 поток)");
            let viewer_cache = viewer_cache;
            let viewer_generation = viewer_generation;
            let last_direction = last_direction;
            while let Ok(task) = load_rx.recv() {
                match task {
                    LoadTask::Photo {
                        index,
                        path,
                        generation: my_gen,
                        skip_depth,
                    } => {
                        // Дренируем очередь: если в канале ещё есть задачи —
                        // берём последнюю (промежуточные пропускаем, не декодируя).
                        let (_, path, my_gen, skip_depth) =
                            drain_to_last(&load_rx, index, path, my_gen, skip_depth);
                        // Устаревшая задача — пропускаем (пользователь ушёл дальше).
                        if viewer_generation.load(Ordering::SeqCst) != my_gen {
                            log::debug!("worker: устарела gen={my_gen}, пропускаю");
                            continue;
                        }
                        let result = load_full_image(path, viewer_cache.clone());
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
                        let _ = load_full_image(path, viewer_cache.clone());
                    }
                }
            }
            log::info!("viewer worker: завершён");
        });
    }
    // Дренирование: берём последнюю задачу Photo из канала, пропуская промежуточные.
    fn drain_to_last(
        rx: &mpsc::Receiver<LoadTask>,
        mut index: i32,
        mut path: std::path::PathBuf,
        mut generation: u64,
        mut skip_depth: u32,
    ) -> (i32, std::path::PathBuf, u64, u32) {
        while let Ok(next) = rx.try_recv() {
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
        (index, path, generation, skip_depth)
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
        load_tx: mpsc::Sender<LoadTask>,
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

        /// ЕДИНЫЙ переход к фото (4.3i): инкрементирует поколение, мгновенно
        /// пишет миниатюру в единый viewer-image и запрашивает чёткое фото
        /// ТОЙ ЖЕ генерацией. Открытие, next/prev и скип используют только
        /// эту схему — она существует в одном месте (DRY), и все записи в
        /// слой сверяются с одним поколением.
        fn switch_to_photo(&self, index: i32, skip_depth: u32) {
            let my_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            self.show_thumbnail(index, my_gen);
            self.request_load(index, my_gen, skip_depth);
        }

        /// Мгновенно показывает миниатюру фото по индексу (моргание).
        /// 4.3i — единый слой: миниатюра пишется в viewer-image (второго слоя
        /// больше нет). Как любая запись в слой, сверяет поколение: устаревший
        /// вызов не перезапишет кадр более нового перехода.
        /// Миниатюры уже в памяти (модель галереи) — показ без задержки.
        fn show_thumbnail(&self, index: i32, my_gen: u64) {
            use slint::Model; // trait Model::row_data
            if self.generation.load(Ordering::SeqCst) != my_gen {
                return; // переход неактуален — слой принадлежит новому поколению
            }
            if let Some(w) = self.ui_weak.upgrade() {
                let thumbs = w.get_thumbnails();
                match thumbs.row_data(index as usize) {
                    Some(item) => {
                        // Размеры для клампа зума: у миниатюры то же соотношение
                        // сторон, что у полного фото, поэтому contain-fit считается
                        // корректно ещё до прихода чёткого кадра (DPI сокращается).
                        let sz = item.image.size();
                        w.set_viewer_img_w(sz.width as f32);
                        w.set_viewer_img_h(sz.height as f32);
                        w.set_viewer_image(item.image);
                    }
                    // Миниатюры нет (модель рассинхронизирована) — чистим слой,
                    // чтобы не показывать кадр предыдущего перехода.
                    None => w.set_viewer_image(slint::Image::default()),
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
                        // 4.3g — размеры картинки для клампа «край в край» (contain-fit).
                        // size() — физические px; в формуле contain_scale DPI сокращается.
                        let sz = img.size();
                        w.set_viewer_img_w(sz.width as f32);
                        w.set_viewer_img_h(sz.height as f32);
                        w.set_viewer_image(img);
                        w.set_viewer_error("".into()); // 4.3e — успешный показ, гасим плашку.
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
                    }
                    self.enqueue_preload(index);
                }
                return;
            }
            // 3) Мимо кэша — задача воркеру. Миниатюра уже в viewer-image
            //    (единый слой, 4.3i); индикатор «Загрузка» убран (бесполезен и залипал).
            let _ = self.load_tx.send(LoadTask::Photo {
                index,
                path,
                generation: my_gen,
                skip_depth,
            });
        }

        /// Ставит в канал задачи на предзагрузку соседей (index-1, index+1).
        /// Воркер обработает их после текущего фото (низкий приоритет).
        fn enqueue_preload(&self, index: i32) {
            let count = lock_or_recover(&self.photos).len() as i32;
            for ni in [index - 1, index + 1] {
                if ni < 0 || ni >= count {
                    continue;
                }
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
                let _ = self.load_tx.send(LoadTask::Preload { path });
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
        load_tx: load_tx.clone(),
        nav_mode: Rc::new(RefCell::new(NavMode::Edge)), // 4.4a — до края по умолчанию
        shuffle_order: RefCell::new(None),              // 4.4c — строится при random
        shuffle_pos: Cell::new(0),
    });

    /// Загружает полное изображение по пути и возвращает сырые RGBA-данные (Send).
    /// Downscale до VIEWER_MAX_SIZE для скорости. Результат кэшируется.
    /// Декодирование обёрнуто в catch_unwind: битый/нестандартный файл не должен
    /// паниковать и отравлять мьютекс — ошибка возвращается как Err.
    fn load_full_image(
        path: std::path::PathBuf,
        cache: Arc<Mutex<ViewerCache>>,
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
        // Downscale, если фото больше максимума (быстрый фильтр Triangle).
        let img = if img.width() > VIEWER_MAX_SIZE || img.height() > VIEWER_MAX_SIZE {
            log::debug!(
                "load_full_image: downscale {}x{} → ≤{VIEWER_MAX_SIZE}",
                img.width(),
                img.height()
            );
            img.resize(
                VIEWER_MAX_SIZE,
                VIEWER_MAX_SIZE,
                image::imageops::FilterType::Triangle,
            )
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

    /// 4.4b — ПОРЦИОННАЯ загрузка миниатюр в галерею (чинит «тупит 30-60 сек»
    /// на 10k фото). Первый батч (FIRST_BATCH=200) заменяет модель — сетка
    /// оживает сразу; остальные добавляются батчами по BATCH=150 в фоне
    /// через invoke_from_event_loop, между батчами UI успевает отрисовать
    /// кадр (не давится одним куском на 10k карточек).
    /// gallery_gen — поколение галереи: батчи старого проекта отменяются,
    /// если открыли другой (иначе чужие миниатюры подмешаются в сетку).
    /// Хэндл VecModel (не-Send) живёт в UI-потоке — фон добирается до него
    /// через invoke_from_event_loop, где берёт модель из UI и downcast'ит.
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
                        if is_first {
                            w.set_thumbnails(slint::ModelRc::new(slint::VecModel::from(items)));
                        } else {
                            // Добавляем в ТУ ЖЕ модель: скролл не сбрасывается.
                            use slint::Model;
                            if let Some(model) = w
                                .get_thumbnails()
                                .as_any()
                                .downcast_ref::<slint::VecModel<ThumbItem>>()
                            {
                                for it in items {
                                    model.push(it);
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

    // --- Viewer колбэги ---
    // Открыть фото по индексу.
    // ВАЖНО: именно здесь включаем ПОЛНОЭКРАННЫЙ режим (как в играх) —
    // окно разворачивается на текущий монитор. Само приложение в обычном
    // оконном режиме; на весь экран уходит только просмотр фото.
    {
        let ctx = viewer_ctx.clone();
        ui.on_open_viewer(move |index| {
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
            ctx.switch_to_photo(index, 0);
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
                    let Some((np, photo)) = nav_random_next(order, ctx.shuffle_pos.get(), 1) else {
                        return; // край перемешанного списка — стоп
                    };
                    ctx.shuffle_pos.set(np);
                    photo
                }
                _ => {
                    let Some(idx) = nav_next(cur, count, 1, mode) else {
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
            ctx.switch_to_photo(idx, 0);
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
                    let Some((np, photo)) = nav_random_next(order, ctx.shuffle_pos.get(), -1)
                    else {
                        return; // начало перемешанного списка — стоп
                    };
                    ctx.shuffle_pos.set(np);
                    photo
                }
                _ => {
                    let Some(idx) = nav_next(cur, count, -1, mode) else {
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
            ctx.switch_to_photo(idx, 0);
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
                match nav_random_next(order, ctx.shuffle_pos.get(), dir_list) {
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
                let idx = next_index;
                if idx < 0 || idx >= count {
                    // Достигли края и дальше годных нет — показываем плашку.
                    log::error!("request-skip: край списка (idx={idx}, count={count}) — плашка");
                    if let Some(w) = ctx.ui_weak.upgrade() {
                        w.set_viewer_image(slint::Image::default());
                        w.set_viewer_error("Файл не открывается".into());
                    }
                    return;
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
            ctx.switch_to_photo(idx, skip_depth as u32);
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
