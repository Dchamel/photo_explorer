// Сборочный скрипт: компилирует .slint-файл разметки интерфейса.
// Оба компонента (LockScreen и PinPad) находятся в одном файле lock.slint.
//
// 4.6-polish — иконка приложения: вшиваем assets/icon.ico в .exe
// (Windows-ресурс). Иконка появляется в Проводнике, панели задач и
// заголовке окна — exe больше не «безликий». Сам icon.ico генерируется
// один раз утилитой examples/gen_icon.rs (рисуем кодом через image-крейт)
// и коммитится; при желании файл можно заменить на красивый — сборка
// подхватит его как есть.
fn main() {
    slint_build::compile("src/ui/views/lock.slint").expect("Slint компиляция не удалась");

    // Иконка — только под Windows (winresource работает с PE-ресурсами).
    // ВАЖНО: icon.ico должен существовать ДО сборки (генерится один раз
    // утилитой examples/gen_icon.rs). Если файла нет — НЕ падаем, а
    // пропускаем вшивание (сборка без иконки лучше упавшей сборки;
    // предупреждение в логе сборки подскажет, что сделать).
    if cfg!(windows) {
        let icon = std::path::Path::new("assets/icon.ico");
        if icon.exists() {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("assets/icon.ico");
            res.compile().expect("вшить иконку в exe");
        } else {
            println!(
                "cargo:warning=assets/icon.ico не найден — exe будет без иконки. \
                 Запустите: cargo run --example gen_icon"
            );
        }
    }
}
