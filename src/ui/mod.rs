//! Слой интерфейса на Slint.
//!
//! Экран блокировки (одно окно): точки PIN + встроенная клавиатура + автовход.
//! Биометрия запускается в ОТДЕЛЬНОМ потоке (op.get() блокирующий) и результат
//! возвращается в UI-поток через slint::invoke_from_event_loop. Флаг «идёт» защищает
//! от повторных запросов (иначе дедлок/зависание).

slint::include_modules!();

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::core::ports::{AuthOutcome, Authenticator};
use crate::security::SecurityService;

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
    use crate::core::models::{Photo, ScanMode};
    use crate::core::ports::{PhotoScanner, ThumbnailProvider};
    use crate::scanner::FileSystemScanner;
    use crate::thumbnails::FileSystemThumbnailProvider;

    /// LRU-кэш полных изображений в памяти: путь -> (RGBA-данные, ширина, высота).
    type ViewerCache = lru::LruCache<std::path::PathBuf, (Vec<u8>, u32, u32)>;

    let ui = GalleryScreen::new()?;
    // Полноэкранный режим на текущем мониторе.
    ui.window().set_fullscreen(true);

    // Сканер и поставщик миниатюр. Кэш миниатюр — в temp.
    let cache_dir = std::env::temp_dir().join("photo_explorer_thumbs_cache");
    let scanner = Arc::new(FileSystemScanner::new());
    let thumbs = Arc::new(FileSystemThumbnailProvider::new(cache_dir, 160));

    // Список Photo (с путями к оригиналам) — нужен для viewer. Arc<Mutex> — доступ из потоков.
    let photos: Arc<Mutex<Vec<Photo>>> = Arc::new(Mutex::new(Vec::new()));

    // LRU-кэш полных изображений в памяти (быстрый повторный просмотр).
    let viewer_cache: Arc<Mutex<ViewerCache>> =
        Arc::new(Mutex::new(ViewerCache::new(20.try_into().unwrap())));

    // Максимальная сторона изображения для viewer (downscale до размера экрана).
    const VIEWER_MAX_SIZE: u32 = 2560;

    /// Загружает полное изображение по пути и возвращает сырые RGBA-данные (Send).
    /// Downscale до VIEWER_MAX_SIZE для скорости. Результат кэшируется.
    fn load_full_image(
        path: std::path::PathBuf,
        cache: Arc<Mutex<ViewerCache>>,
    ) -> Result<(Vec<u8>, u32, u32), String> {
        // Проверяем кэш.
        if let Some(cached) = cache.lock().unwrap().get(&path).cloned() {
            return Ok(cached);
        }
        let img = image::open(&path).map_err(|e| format!("Ошибка загрузки: {e}"))?;
        // Downscale, если фото больше максимума (быстрый фильтр Triangle).
        let img = if img.width() > VIEWER_MAX_SIZE || img.height() > VIEWER_MAX_SIZE {
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
        // Сохраняем в кэш.
        cache.lock().unwrap().put(path, (data.clone(), w, h));
        Ok((data, w, h))
    }

    /// Загружает полное изображение по индексу в фоновом потоке, результат — в UI.
    /// Если уже в кэше — мгновенно. После — предзагружает соседей.
    fn load_viewer_image(
        photos: Arc<Mutex<Vec<Photo>>>,
        index: i32,
        ui_weak: slint::Weak<GalleryScreen>,
        cache: Arc<Mutex<ViewerCache>>,
    ) {
        let idx = index as usize;
        let path = {
            let p = photos.lock().unwrap();
            match p.get(idx) {
                Some(ph) => ph.path.clone(),
                None => return,
            }
        };
        // Быстрая проверка кэша — если есть, показываем сразу (без потока).
        if let Some(cached) = cache.lock().unwrap().get(&path).cloned() {
            if let Some(w) = ui_weak.upgrade() {
                let (data, w_px, h_px) = cached;
                let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w_px, h_px);
                let pixels = buf.make_mut_slice();
                for (dst, chunk) in pixels.iter_mut().zip(data.as_chunks::<4>().0) {
                    dst.r = chunk[0];
                    dst.g = chunk[1];
                    dst.b = chunk[2];
                    dst.a = chunk[3];
                }
                w.set_viewer_image(slint::Image::from_rgba8(buf));
            }
            // Предзагрузка соседей в фоне.
            preload_neighbors(photos.clone(), index, cache.clone());
            return;
        }
        if let Some(w) = ui_weak.upgrade() {
            w.set_viewer_loading(true);
        }
        std::thread::spawn(move || {
            let result = load_full_image(path, cache.clone());
            let ui_weak = ui_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = ui_weak.upgrade() {
                    w.set_viewer_loading(false);
                    match result {
                        Ok((data, w_px, h_px)) => {
                            let mut buf =
                                slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w_px, h_px);
                            let pixels = buf.make_mut_slice();
                            for (dst, chunk) in pixels.iter_mut().zip(data.as_chunks::<4>().0) {
                                dst.r = chunk[0];
                                dst.g = chunk[1];
                                dst.b = chunk[2];
                                dst.a = chunk[3];
                            }
                            w.set_viewer_image(slint::Image::from_rgba8(buf));
                        }
                        Err(e) => w.set_error_text(e.into()),
                    }
                }
            });
            // Предзагрузка соседей в фоне (после текущего).
            preload_neighbors(photos, index, cache);
        });
    }

    /// Предзагружает соседние фото (index-1, index+1) в кэш в фоновом потоке.
    fn preload_neighbors(
        photos: Arc<Mutex<Vec<Photo>>>,
        index: i32,
        cache: Arc<Mutex<ViewerCache>>,
    ) {
        let count = photos.lock().unwrap().len() as i32;
        if count == 0 {
            return;
        }
        let neighbors = [index - 1, index + 1];
        for ni in neighbors {
            if ni < 0 || ni >= count {
                continue;
            }
            let path = {
                let p = photos.lock().unwrap();
                match p.get(ni as usize) {
                    Some(ph) => ph.path.clone(),
                    None => continue,
                }
            };
            // Уже в кэше — пропускаем.
            if cache.lock().unwrap().contains(&path) {
                continue;
            }
            let cache = cache.clone();
            std::thread::spawn(move || {
                let _ = load_full_image(path, cache);
            });
        }
    }

    /// Запускает выбор папки + скан + генерацию миниатюр в фоновом потоке.
    fn scan_and_show(
        scanner: Arc<FileSystemScanner>,
        thumbs: Arc<FileSystemThumbnailProvider>,
        ui_weak: slint::Weak<GalleryScreen>,
        photos: Arc<Mutex<Vec<Photo>>>,
        recursive: bool,
    ) {
        if let Some(w) = ui_weak.upgrade() {
            w.set_scanning(true);
            w.set_error_text("".into());
        }
        std::thread::spawn(move || {
            let folder = rfd::FileDialog::new()
                .set_title("Выберите папку с фотографиями")
                .pick_folder();

            let result: Result<(Vec<Photo>, Vec<crate::core::models::Thumbnail>), String> =
                (|| {
                    let folder = folder.ok_or("Папка не выбрана")?;
                    let mode = if recursive {
                        ScanMode::FolderRecursive(folder)
                    } else {
                        ScanMode::SingleFolder(folder)
                    };
                    let photos = scanner
                        .scan(&mode)
                        .map_err(|e| format!("Ошибка скана: {e}"))?;
                    let thumb_results = thumbs.thumbnails(&photos);
                    let out: Vec<_> = thumb_results.into_iter().flatten().collect();
                    Ok((photos, out))
                })();

            let ui_weak = ui_weak.clone();
            let photos_for_store = photos.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = ui_weak.upgrade() {
                    w.set_scanning(false);
                    match result {
                        Ok((scanned_photos, thumbs)) => {
                            let count = thumbs.len();
                            // Сохраняем пути оригиналов для viewer.
                            *photos_for_store.lock().unwrap() = scanned_photos;
                            let items: Vec<ThumbItem> = thumbs
                                .into_iter()
                                .map(|t| {
                                    let mut buf =
                                        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(
                                            t.width, t.height,
                                        );
                                    let pixels = buf.make_mut_slice();
                                    for (dst, chunk) in
                                        pixels.iter_mut().zip(t.data.as_chunks::<4>().0)
                                    {
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
                                        source_path: t
                                            .source_path
                                            .to_string_lossy()
                                            .to_string()
                                            .into(),
                                        image: slint::Image::from_rgba8(buf),
                                    }
                                })
                                .collect();
                            let model = slint::VecModel::from(items);
                            w.set_thumbnails(slint::ModelRc::new(model));
                            w.set_status_text(format!("Найдено фото: {count}").into());
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
        let ui_weak = ui.as_weak();
        let photos = photos.clone();
        ui.on_pick_folder_flat(move || {
            scan_and_show(
                scanner.clone(),
                thumbs.clone(),
                ui_weak.clone(),
                photos.clone(),
                false,
            );
        });
    }
    // Колбэк «Папка с подпапками».
    {
        let scanner = scanner.clone();
        let thumbs = thumbs.clone();
        let ui_weak = ui.as_weak();
        let photos = photos.clone();
        ui.on_pick_folder_recursive(move || {
            scan_and_show(
                scanner.clone(),
                thumbs.clone(),
                ui_weak.clone(),
                photos.clone(),
                true,
            );
        });
    }

    // --- Viewer колбэги ---
    // Открыть фото по индексу.
    {
        let photos = photos.clone();
        let ui_weak = ui.as_weak();
        let viewer_cache = viewer_cache.clone();
        ui.on_open_viewer(move |index| {
            if let Some(w) = ui_weak.upgrade() {
                w.set_viewer_index(index);
                w.set_show_viewer(true);
            }
            load_viewer_image(photos.clone(), index, ui_weak.clone(), viewer_cache.clone());
        });
    }
    // Закрыть viewer.
    {
        let ui_weak = ui.as_weak();
        ui.on_close_viewer(move || {
            if let Some(w) = ui_weak.upgrade() {
                w.set_show_viewer(false);
            }
        });
    }
    // Следующее фото.
    {
        let photos = photos.clone();
        let ui_weak = ui.as_weak();
        let viewer_cache = viewer_cache.clone();
        ui.on_viewer_next(move || {
            if let Some(w) = ui_weak.upgrade() {
                let count = photos.lock().unwrap().len() as i32;
                if count == 0 {
                    return;
                }
                let mut idx = w.get_viewer_index() + 1;
                if idx >= count {
                    idx = 0;
                }
                w.set_viewer_index(idx);
            }
            let idx = ui_weak.upgrade().map(|w| w.get_viewer_index()).unwrap_or(0);
            load_viewer_image(photos.clone(), idx, ui_weak.clone(), viewer_cache.clone());
        });
    }
    // Предыдущее фото.
    {
        let photos = photos.clone();
        let ui_weak = ui.as_weak();
        let viewer_cache = viewer_cache.clone();
        ui.on_viewer_prev(move || {
            if let Some(w) = ui_weak.upgrade() {
                let count = photos.lock().unwrap().len() as i32;
                if count == 0 {
                    return;
                }
                let mut idx = w.get_viewer_index() - 1;
                if idx < 0 {
                    idx = count - 1;
                }
                w.set_viewer_index(idx);
            }
            let idx = ui_weak.upgrade().map(|w| w.get_viewer_index()).unwrap_or(0);
            load_viewer_image(photos.clone(), idx, ui_weak.clone(), viewer_cache.clone());
        });
    }

    ui.run()
}
