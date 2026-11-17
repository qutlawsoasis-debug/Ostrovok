//! Установка «по-настоящему»: остров копирует себя в %LOCALAPPDATA%\Programs\Ostrovok, ставит ярлык
//! в меню «Пуск» и переносит туда автозапуск. Права администратора не нужны — всё в профиле пользователя.
//! Делается только по кнопке в настройках.

use std::path::{Path, PathBuf};

/// Папка, куда ставится остров.
pub fn home() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs").join("Ostrovok"))
}

fn start_menu_link() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join(r"Microsoft\Windows\Start Menu\Programs").join("Островок.lnk"))
}

/// Пути Windows сравниваются без учёта регистра и косых черт.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    norm(a) == norm(b)
}

/// Остров уже запущен из своей папки.
pub fn installed() -> bool {
    match (std::env::current_exe(), home()) {
        (Ok(exe), Some(home)) => exe.parent().is_some_and(|p| same_dir(p, &home)),
        _ => false,
    }
}

/// Копирует остров на место, ставит ярлык и переносит автозапуск. Возвращает путь к новой копии;
/// вызывающий запускает её и выходит.
pub fn install() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let home = home().ok_or("не нашлась папка программ пользователя")?;
    std::fs::create_dir_all(&home).map_err(|e| format!("не удалось создать папку: {e}"))?;
    let target = home.join("Ostrovok.exe");
    std::fs::copy(&exe, &target).map_err(|e| format!("не удалось скопировать: {e}"))?;
    if let Some(link) = start_menu_link() {
        if let Err(e) = shortcut(&target, &link) {
            return Err(format!("остров скопирован, но ярлык не создался: {e}"));
        }
    }
    // в проверочном запуске реестр не трогаем
    if std::env::var_os("OSTROVOK_TEST").is_none() && crate::store::autostart_enabled() {
        crate::store::set_autostart_path(&target);
    }
    Ok(target)
}

fn shortcut(target: &Path, link: &Path) -> windows::core::Result<()> {
    use windows::core::{Interface, HSTRING};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        sl.SetPath(&HSTRING::from(target.as_os_str()))?;
        if let Some(dir) = target.parent() {
            sl.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        }
        sl.SetDescription(&HSTRING::from("Островок — живая панель в верхней части экрана"))?;
        sl.SetIconLocation(&HSTRING::from(target.as_os_str()), 0)?;
        sl.cast::<IPersistFile>()?.Save(&HSTRING::from(link.as_os_str()), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compares_windows_paths() {
        assert!(same_dir(Path::new(r"C:\Users\A\AppData\Local\Programs\Ostrovok"), Path::new(r"c:\users\a\appdata\local\programs\ostrovok\")));
        assert!(same_dir(Path::new("C:/x/y"), Path::new(r"C:\X\Y")));
        assert!(!same_dir(Path::new(r"C:\x\y"), Path::new(r"C:\x\yz")));
    }
}
