//! 4.6-polish — генератор иконки приложения (рисуем кодом, без внешних
//! файлов). Запускается ВРУЧНУЮ один раз (см. комментарий ниже), результат
//! — assets/icon.ico — коммитится в репозиторий.
//!
//! ЗАЧЕМ: exe без иконки выглядит «пустым» (заказчик: «стремно когда у
//! экзешника картинки нет»). Джар/Солдат не могут создать бинарный .ico
//! с картинкой «из текста» — поэтому рисуем ПРОСТУЮ, но аккуратную иконку
//! кодом: фирменный синий (Theme.accent #2f81f7-оттенок), скруглённый
//! квадрат, белая рамка кадра и «блик» — читается как «просмотрщик фото».
//! Позже заказчик может подменить assets/icon.ico на красивую —
//! сборка подхватит файл как есть (KISS).
//!
//! КАК ЗАПУСКАТЬ (не часть cargo build — утилита для разработчика):
//!   cargo run --example gen_icon            — если положить в examples/
//!   либо: rustc tools/gen_icon.rs -o gen_icon && ./gen_icon
//! Мы кладём этот файл в examples/, чтобы cargo run --example работал
//! без отдельного бинарника в workspace.

use image::{DynamicImage, Rgba, RgbaImage};

/// Фирменный синий (акцент темы Photo Explorer).
const ACCENT: Rgba<u8> = Rgba([47, 129, 247, 255]);
/// Более тёмный синий для нижней «стопки» (глубина).
const ACCENT_DARK: Rgba<u8> = Rgba([26, 77, 127, 255]);
/// Белый для рамки кадра.
const WHITE: Rgba<u8> = Rgba([255, 255, 255, 255]);
/// Полупрозрачный белый для блика.
const GLOSS: Rgba<u8> = Rgba([255, 255, 255, 90]);

/// Рисует иконку размера size×size (квадрат, рисуем в RGBA).
///
/// Композиция (просто и узнаваемо — «стопка фото»):
/// - фон: скруглённый квадрат фирменного синего;
/// - сзади: два «листа» стопки (темнее, чуть смещены) — намёк на галерею;
/// - спереди: белый прямоугольник-рамка кадра с синей «фотографией»
///   внутри и бликом — как снимок на просмотре.
fn draw_icon(size: u32) -> RgbaImage {
    let mut img = RgbaImage::new(size, size);
    let s = size as f32;

    // Скруглённый квадрат: радиус ~22% стороны (как у современных иконок).
    let radius = s * 0.22;
    let in_round = |x: f32, y: f32| -> bool {
        // Внутри квадрата со скруглением: центр угла = (radius, radius) и
        // зеркальные; если точка в угловой зоне — проверяем радиус,
        // иначе — обычная точка квадрата.
        let corner_x = if x < radius {
            radius
        } else if x > s - radius {
            s - radius
        } else {
            x
        };
        let corner_y = if y < radius {
            radius
        } else if y > s - radius {
            s - radius
        } else {
            y
        };
        if corner_x == x || corner_y == y {
            return true; // не в угловой зоне — обычная точка квадрата
        }
        let ddx = x - corner_x;
        let ddy = y - corner_y;
        ddx * ddx + ddy * ddy <= radius * radius
    };

    // 1) Фон — скруглённый квадрат фирменного синего.
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            if in_round(fx, fy) {
                img.put_pixel(x, y, ACCENT);
            }
        }
    }

    // 2) «Стопка» сзади: два тёмных листа, смещённых вниз-вправо.
    let sheet = |img: &mut RgbaImage, off: f32, color: Rgba<u8>| {
        let m = s * 0.16; // поля
        let x0 = m + off;
        let y0 = m + off;
        let x1 = s - m + off;
        let y1 = s - m + off;
        for y in 0..size {
            for x in 0..size {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                if fx >= x0 && fx <= x1 && fy >= y0 && fy <= y1 {
                    // Не рисуем поверх фона за пределами скругления.
                    if in_round(fx, fy) {
                        img.put_pixel(x, y, color);
                    }
                }
            }
        }
    };
    sheet(&mut img, s * 0.06, ACCENT_DARK);
    sheet(&mut img, s * 0.03, ACCENT_DARK);

    // 3) Рамка кадра: белый прямоугольник по центру (лист стопки спереди).
    let m = s * 0.16;
    let x0 = m;
    let y0 = m;
    let x1 = s - m;
    let y1 = s - m;
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            if fx >= x0 && fx <= x1 && fy >= y0 && fy <= y1 && in_round(fx, fy) {
                img.put_pixel(x, y, WHITE);
            }
        }
    }

    // 4) «Фотография» внутри рамки: синий прямоугольник с отступом.
    let pad = s * 0.24;
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            if fx >= pad && fx <= s - pad && fy >= pad && fy <= s - pad {
                img.put_pixel(x, y, ACCENT);
            }
        }
    }

    // 5) Блик: верхняя половина «фотографии» чуть светлее (стеклянный
    //    эффект), читается как отражение на снимке. Rgba — кортежная
    //    структура: компоненты через .0[i], альфа — GLOSS.0[3].
    let gloss = GLOSS.0;
    for y in 0..size {
        let fy = y as f32 + 0.5;
        if fy >= pad && fy <= s / 2.0 {
            for x in 0..size {
                let fx = x as f32 + 0.5;
                if fx >= pad && fx <= s - pad {
                    let p = img.get_pixel(x, y).0;
                    let a = gloss[3] as u16;
                    let blended = Rgba([
                        (p[0] as u16 + (gloss[0] as u16 * a / 255)) as u8,
                        (p[1] as u16 + (gloss[1] as u16 * a / 255)) as u8,
                        (p[2] as u16 + (gloss[2] as u16 * a / 255)) as u8,
                        255,
                    ]);
                    img.put_pixel(x, y, blended);
                }
            }
        }
    }

    img
}

fn main() {
    // Мультиразмерная иконка: 16/32/48/256 — стандарт для Windows.
    // Крейт image при save(".ico") пишет ТОЛЬКО ОДИН кадр (первый), а
    // Windows-иконка должна содержать все размеры в одном файле.
    // Поэтому собираем ICO-контейнер вручную: формат прост —
    // 6-байтовый заголовок + N директорий по 16 байт + PNG-данные.
    let sizes = [16u32, 32, 48, 256];
    let mut pngs: Vec<Vec<u8>> = Vec::new();
    for &s in &sizes {
        let img = DynamicImage::ImageRgba8(draw_icon(s));
        let mut png = Vec::<u8>::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("PNG-кадр иконки");
        pngs.push(png);
    }

    let mut ico = Vec::<u8>::new();
    // Заголовок ICONDIR: reserved=0, type=1 (icon), count=N.
    ico.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    ico.extend_from_slice(&(pngs.len() as u16).to_le_bytes()); // count
                                                               // Директории: смещения считаем после заголовка и всех директорий.
    let header_len = 6 + 16 * pngs.len();
    let mut offset = header_len;
    for (i, png) in pngs.iter().enumerate() {
        let s = sizes[i];
        let dim = if s >= 256 { 0u8 } else { s as u8 }; // 256 = 0 в ICO
        ico.push(dim); // width
        ico.push(dim); // height
        ico.push(0); // палитра (нет)
        ico.push(0); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // цветовые планы
        ico.extend_from_slice(&32u16.to_le_bytes()); // бит на пиксель
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes()); // размер
        ico.extend_from_slice(&(offset as u32).to_le_bytes()); // смещение
        offset += png.len();
    }
    for png in &pngs {
        ico.extend_from_slice(png);
    }

    // Каталог assets/ создаём при необходимости.
    std::fs::create_dir_all("assets").expect("создать assets/");
    let out = std::path::Path::new("assets").join("icon.ico");
    std::fs::write(&out, &ico).expect("записать icon.ico");
    println!("Иконка сохранена: {} ({} байт)", out.display(), ico.len());
    println!("Размеры: {sizes:?} (мультиразмерный .ico, PNG-кадры)");
}
