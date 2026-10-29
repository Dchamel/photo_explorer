//! Адаптер сессионного хранилища (4.7 «Проекты и недавнее»).
//!
//! Реализация порта [`SessionStore`] поверх JSON-файлов на диске.
//!
//! Формат (KISS: один проект = один файл):
//! - `<dir>/index.json` — лёгкий список всех сессий (id, имя, путь, время,
//!   флаг проекта) БЕЗ списков файлов. Стартовый экран «Проекты / Недавние»
//!   рисуется из него мгновенно, не читая полные сессии с 8000 фото.
//! - `<dir>/<id>.json` — полная сессия: метаданные + список `Photo` +
//!   fingerprint. Читается только при открытии конкретной сессии.
//!
//! ГЛАВНОЕ требование заказчика: открытие сессии должно быть БЫСТРЫМ —
//! не медленнее нового скана. Поэтому: чтение одного JSON (килобайты/мегабайты)
//! вместо обхода 8000 файлов с генерацией миниатюр (минуты).
//!
//! Fingerprint папки — по МЕТАДАННЫМ без чтения тел файлов:
//! `(относительный путь, размер, mtime)` каждого фото, отсортированный список,
//! быстрый хеш (FNV-1a 64). Обход метаданных — секунды против минут полного скана.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use crate::core::models::{Photo, Session, SessionSummary};
use crate::core::ports::{SessionError, SessionStore};

/// Считает fingerprint списка фото: хеш отсортированных метаданных.
///
/// Чистая функция — легко тестировать. Сортировка гарантирует, что порядок
/// обхода ФС (нестабильный между запусками) не меняет отпечаток.
pub fn fingerprint_of(photos: &[Photo]) -> String {
    // Сортируем по пути — отпечаток не зависит от порядка обхода.
    let mut entries: Vec<(String, u64, u64)> = photos
        .iter()
        .map(|p| {
            (
                p.path.to_string_lossy().to_lowercase(),
                p.size_bytes,
                p.modified,
            )
        })
        .collect();
    entries.sort();

    let mut hasher = DefaultHasher::new();
    for (path, size, modified) in entries {
        path.hash(&mut hasher);
        size.hash(&mut hasher);
        modified.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

/// Вычисляет инкрементальную разницу между старым и новым списком фото.
///
/// Возвращает только НОВЫЕ фото (те, которых не было в старом списке, или у
/// которых изменились размер/mtime). Это основа инкрементального досскана:
/// «добавили 8 файлов в папку из 8000 — обрабатываем только эти 8».
pub fn diff_photos(old: &[Photo], new: &[Photo]) -> Vec<Photo> {
    use std::collections::HashSet;
    let old_keys: HashSet<(String, u64, u64)> = old
        .iter()
        .map(|p| {
            (
                p.path.to_string_lossy().to_lowercase(),
                p.size_bytes,
                p.modified,
            )
        })
        .collect();
    new.iter()
        .filter(|p| {
            !old_keys.contains(&(
                p.path.to_string_lossy().to_lowercase(),
                p.size_bytes,
                p.modified,
            ))
        })
        .cloned()
        .collect()
}

/// JSON-хранилище сессий поверх папки на диске.
pub struct JsonSessionStore {
    dir: PathBuf,
}

impl JsonSessionStore {
    /// Создаёт хранилище в указанной папке (создаёт папку при необходимости).
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// Путь к index.json (лёгкий список сессий).
    fn index_path(&self) -> PathBuf {
        self.dir.join("index.json")
    }

    /// Путь к файлу конкретной сессии.
    fn session_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    /// Читает index.json; если файла нет — возвращает пустой список.
    fn read_index(&self) -> Result<Vec<SessionSummary>, SessionError> {
        let path = self.index_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| SessionError::IoError(format!("чтение index.json: {e}")))?;
        serde_json::from_str(&text).map_err(|e| SessionError::Corrupted(format!("index.json: {e}")))
    }

    /// Записывает index.json (атомарно: сначала tmp, потом rename).
    fn write_index(&self, summaries: &[SessionSummary]) -> Result<(), SessionError> {
        let text = serde_json::to_string_pretty(summaries)
            .map_err(|e| SessionError::Corrupted(format!("сериализация index.json: {e}")))?;
        let path = self.index_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| SessionError::IoError(format!("создание папки сессий: {e}")))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)
            .map_err(|e| SessionError::IoError(format!("запись index.json: {e}")))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| SessionError::IoError(format!("переименование index.json: {e}")))?;
        Ok(())
    }
}

impl SessionStore for JsonSessionStore {
    fn summaries(&self) -> Result<Vec<SessionSummary>, SessionError> {
        let mut list = self.read_index()?;
        // Свежие сверху (по времени последнего открытия).
        list.sort_by_key(|s| std::cmp::Reverse(s.last_opened));
        Ok(list)
    }

    fn load(&self, id: &str) -> Result<Session, SessionError> {
        let path = self.session_path(id);
        if !path.exists() {
            return Err(SessionError::NotFound(id.to_string()));
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| SessionError::IoError(format!("чтение {id}.json: {e}")))?;
        serde_json::from_str(&text).map_err(|e| SessionError::Corrupted(format!("{id}.json: {e}")))
    }

    fn save(&self, session: &Session) -> Result<(), SessionError> {
        // 1) Пишем файл сессии (атомарно через tmp).
        let text = serde_json::to_string_pretty(session)
            .map_err(|e| SessionError::Corrupted(format!("сериализация {}: {e}", session.id)))?;
        let path = self.session_path(&session.id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| SessionError::IoError(format!("создание папки сессий: {e}")))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)
            .map_err(|e| SessionError::IoError(format!("запись {}: {e}", session.id)))?;
        std::fs::rename(&tmp, &path)
            .map_err(|e| SessionError::IoError(format!("переименование {}: {e}", session.id)))?;

        // 2) Обновляем index.json (upsert по id).
        let mut list = self.read_index()?;
        let summary = SessionSummary::from(session);
        match list.iter().position(|s| s.id == summary.id) {
            Some(i) => list[i] = summary,
            None => list.push(summary),
        }
        self.write_index(&list)
    }

    fn delete(&self, id: &str) -> Result<(), SessionError> {
        let path = self.session_path(id);
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| SessionError::IoError(format!("удаление {id}.json: {e}")))?;
        }
        let mut list = self.read_index()?;
        let before = list.len();
        list.retain(|s| s.id != id);
        if list.len() != before {
            self.write_index(&list)?;
        }
        Ok(())
    }

    fn find_by_folder(&self, folder: &Path, mode: &str) -> Result<Option<Session>, SessionError> {
        // Ищем в лёгком индексе по (путь, режим) — быстро, без чтения полных файлов.
        let list = self.read_index()?;
        let folder_str = folder.to_string_lossy().to_lowercase();
        let hit = list
            .iter()
            .find(|s| s.folder.to_string_lossy().to_lowercase() == folder_str)
            .map(|s| s.id.clone());
        match hit {
            Some(id) => {
                // Режим сверяем уже в полной сессии (в summary его нет — KISS).
                let session = self.load(&id)?;
                if session.mode == mode {
                    Ok(Some(session))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn photo(path: &str, size: u64, modified: u64) -> Photo {
        Photo {
            path: PathBuf::from(path),
            file_name: path.rsplit(['\\', '/']).next().unwrap_or(path).to_string(),
            size_bytes: size,
            modified,
        }
    }

    #[test]
    fn fingerprint_stable_regardless_of_order() {
        let a = vec![photo("C:/a/1.jpg", 10, 100), photo("C:/a/2.jpg", 20, 200)];
        let b = vec![photo("C:/a/2.jpg", 20, 200), photo("C:/a/1.jpg", 10, 100)];
        assert_eq!(fingerprint_of(&a), fingerprint_of(&b));
    }

    #[test]
    fn fingerprint_changes_on_size_or_mtime_or_set() {
        let base = vec![photo("C:/a/1.jpg", 10, 100), photo("C:/a/2.jpg", 20, 200)];
        let fp = fingerprint_of(&base);
        // Изменился размер.
        assert_ne!(
            fp,
            fingerprint_of(&[photo("C:/a/1.jpg", 11, 100), photo("C:/a/2.jpg", 20, 200)])
        );
        // Изменился mtime.
        assert_ne!(
            fp,
            fingerprint_of(&[photo("C:/a/1.jpg", 10, 101), photo("C:/a/2.jpg", 20, 200)])
        );
        // Файл удалён.
        assert_ne!(fp, fingerprint_of(&[photo("C:/a/1.jpg", 10, 100)]));
        // Файл добавлен.
        assert_ne!(
            fp,
            fingerprint_of(&[
                photo("C:/a/1.jpg", 10, 100),
                photo("C:/a/2.jpg", 20, 200),
                photo("C:/a/3.jpg", 30, 300)
            ])
        );
    }

    #[test]
    fn diff_returns_only_new_or_changed() {
        let old = vec![photo("C:/a/1.jpg", 10, 100), photo("C:/a/2.jpg", 20, 200)];
        let new = vec![
            photo("C:/a/1.jpg", 10, 100), // не изменился
            photo("C:/a/2.jpg", 21, 200), // изменился размер
            photo("C:/a/3.jpg", 30, 300), // новый
        ];
        let diff = diff_photos(&old, &new);
        assert_eq!(diff.len(), 2);
        assert!(diff.iter().any(|p| p.path == *Path::new("C:/a/2.jpg")));
        assert!(diff.iter().any(|p| p.path == *Path::new("C:/a/3.jpg")));
    }
}
