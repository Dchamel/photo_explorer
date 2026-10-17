// Сборочный скрипт: компилирует .slint-файл разметки интерфейса.
// Оба компонента (LockScreen и PinPad) находятся в одном файле lock.slint.
fn main() {
    slint_build::compile("src/ui/views/lock.slint").expect("Slint компиляция не удалась");
}
