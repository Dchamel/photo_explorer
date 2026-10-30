//! Доменные модели приложения.

use std::path::PathBuf;

/// Фотография (минимум для MVP).
///
/// Расширяемость (EXIF, дата съёмки, геолокация) — в бэклоге.
/// 4.7 — `modified` (unix-секунды) нужен для fingerprint папки: сравниваем
/// метаданные без чтения тел файлов. Serde — для сериализации сессии в JSON.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Photo {
    /// Абсолютный путь к файлу изображения.
    pub path: PathBuf,
    /// Имя файла (без пути).
    pub file_name: String,
    /// Размер файла в байтах.
    pub size_bytes: u64,
    /// Время последней модификации файла (unix-секунды).
    pub modified: u64,
}

/// Сессия просмотра папки (4.7 «Проекты и недавние»).
///
/// Один раз отсканировали папку — сохранили список фото; при повторном входе
/// открываем мгновенно из JSON, если папка не менялась (fingerprint совпал).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Session {
    /// Уникальный id сессии (uuid-подобная строка).
    pub id: String,
    /// Имя проекта (для именованных проектов; у recent — имя папки).
    pub name: String,
    /// Путь к отсканированной папке.
    pub folder: PathBuf,
    /// Режим скана (flat / recursive) — строка "flat" | "recursive".
    pub mode: String,
    /// Отпечаток папки на момент сохранения (хеш метаданных).
    pub fingerprint: String,
    /// Список фото на момент сохранения.
    pub photos: Vec<Photo>,
    /// Unix-время последнего открытия сессии.
    pub last_opened: u64,
    /// Является ли именованным проектом (true) или недавним (false).
    pub is_project: bool,
    /// 4.4a — режим листания в просмотрщике: "edge" (до края, стоп) |
    /// "cycle" (циклично по кругу) | "random" (случайный порядок).
    /// serde(default): старые JSON без поля читаются, пустая строка
    /// трактуется как "edge" (по умолчанию — самое предсказуемое).
    #[serde(default)]
    pub nav_mode: String,
}

/// Краткая запись о сессии для стартового экрана (без списка файлов).
///
/// Стартовый экран «Проекты / Недавние» рисуется из лёгкого index.json —
/// без чтения полных сессий (списки из 8000 файлов не нужны, пока не кликнули).
/// 4.7g — cover_path (первое фото, для обложки карточки) и photo_count
/// (счётчик в подписи карточки). serde(default): старые index.json без этих
/// полей продолжают читаться (обложка — пустая, счётчик — 0).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub name: String,
    pub folder: PathBuf,
    pub last_opened: u64,
    pub is_project: bool,
    /// Путь к первому фото сессии (обложка карточки на стартовом экране).
    #[serde(default)]
    pub cover_path: Option<PathBuf>,
    /// Сколько фото в сессии (подпись карточки).
    #[serde(default)]
    pub photo_count: u64,
}

impl From<&Session> for SessionSummary {
    fn from(s: &Session) -> Self {
        Self {
            id: s.id.clone(),
            name: s.name.clone(),
            folder: s.folder.clone(),
            last_opened: s.last_opened,
            is_project: s.is_project,
            cover_path: s.photos.first().map(|p| p.path.clone()),
            photo_count: s.photos.len() as u64,
        }
    }
}

/// Режим сканирования папок.
#[derive(Debug, Clone)]
pub enum ScanMode {
    // NOTE: режим AllDrives временно отключён по решению заказчика (медленный,
    // пока не нужен — KISS). Код сохранён в scanner/mod.rs под комментарием,
    // чтобы можно было включить позже. Рабочие режимы — SingleFolder и FolderRecursive.
    // /// Скан всех папок по доступным дискам.
    // AllDrives,
    /// Скан только конкретной папки (без подпапок).
    SingleFolder(PathBuf),
    /// Скан конкретной папки и всех её подпапок.
    FolderRecursive(PathBuf),
}

/// Миниатюра изображения (готова для показа в UI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    /// Путь к исходному файлу (для связи с [`Photo`]).
    pub source_path: PathBuf,
    /// Ширина миниатюры в пикселях.
    pub width: u32,
    /// Высота миниатюры в пикселях.
    pub height: u32,
    /// Пиксельные данные в формате RGBA (4 байта на пиксель: R, G, B, A).
    pub data: Vec<u8>,
}
