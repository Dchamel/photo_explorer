// Сборочный скрипт: компилирует .slint-файлы разметки интерфейса.
fn main() {
    slint_build::compile("src/ui/views/lock.slint").expect("Slint компиляция не удалась");
}
