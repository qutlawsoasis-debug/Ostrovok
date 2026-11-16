//! Обёртки над Windows: курсор, горячая клавиша, буфер, снимок экрана, распознавание текста, запуск программ.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, HWND, POINT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::GetClipboardSequenceNumber;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// false, если Островок уже запущен.
pub fn single_instance() -> bool {
    unsafe {
        let h = CreateMutexW(None, true, w!("Ostrovok.SingleInstance"));
        let exists = windows::Win32::Foundation::GetLastError() == ERROR_ALREADY_EXISTS;
        std::mem::forget(h);
        !exists
    }
}

/// То же после самообновления: прежняя копия ещё закрывается, поэтому ждём её до восьми секунд.
pub fn wait_single_instance() -> bool {
    for _ in 0..80 {
        unsafe {
            let h = CreateMutexW(None, true, w!("Ostrovok.SingleInstance"));
            if windows::Win32::Foundation::GetLastError() != ERROR_ALREADY_EXISTS {
                std::mem::forget(h);
                return true;
            }
            if let Ok(h) = h {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

pub fn cursor() -> Option<(i32, i32)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok()? };
    Some((p.x, p.y))
}

pub fn screen_width() -> i32 {
    unsafe { GetSystemMetrics(SM_CXSCREEN) }
}

pub fn clipboard_seq() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// Открыть файл, ярлык или ссылку так же, как двойным щелчком.
pub fn open(target: &str) {
    unsafe {
        ShellExecuteW(HWND::default(), w!("open"), &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Поток с глобальной горячей клавишей. Возвращает её название.
pub fn spawn_hotkey(flag: Arc<AtomicBool>, ctx: eframe::egui::Context) -> &'static str {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || unsafe {
        let name = if RegisterHotKey(HWND::default(), 1, MOD_ALT | MOD_NOREPEAT, 0x20).is_ok() {
            "Alt + Space"
        } else if RegisterHotKey(HWND::default(), 1, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, 0x20).is_ok() {
            "Ctrl + Alt + Space"
        } else {
            "не назначена"
        };
        let _ = tx.send(name);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            if msg.message == WM_HOTKEY {
                flag.store(true, Ordering::Relaxed);
                ctx.request_repaint();
            }
        }
    });
    rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap_or("не назначена")
}

/// Нужно ли спрятать остров: активное чужое окно находится на основном экране (там, где остров)
/// и либо занимает его целиком, либо это полноэкранная игра или презентация.
/// Полноэкранное окно на другом мониторе острову не мешает.
pub fn fullscreen_app_active() -> bool {
    use windows::Win32::UI::Shell::{SHQueryUserNotificationState, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN};
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == std::process::id() {
            return false;
        }
        let mut buf = [0u16; 64];
        let n = GetClassNameW(fg, &mut buf) as usize;
        let class = String::from_utf16_lossy(&buf[..n]);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") {
            return false;
        }
        let mut r = windows::Win32::Foundation::RECT::default();
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetWindowRect(fg, &mut r).is_err() || !GetMonitorInfoW(MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() {
            return false;
        }
        let m = mi.rcMonitor;
        // основной экран начинается в точке (0, 0)
        if m.left != 0 || m.top != 0 {
            return false;
        }
        let covers = r.left == m.left && r.top == m.top && r.right == m.right && r.bottom == m.bottom;
        let game = matches!(SHQueryUserNotificationState(), Ok(st) if st == QUNS_RUNNING_D3D_FULL_SCREEN || st == QUNS_PRESENTATION_MODE);
        covers || game
    }
}

/// Снимок монитора, на котором сейчас курсор. Возвращает ширину, высоту и RGBA.
pub fn capture_screen() -> Result<(usize, usize, Vec<u8>), String> {
    unsafe {
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if !GetMonitorInfoW(mon, &mut mi).as_bool() {
            return Err("не удалось определить монитор".into());
        }
        let r = mi.rcMonitor;
        let (w, h) = (r.right - r.left, r.bottom - r.top);

        let screen = GetDC(HWND::default());
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp);
        let blt = BitBlt(mem, 0, 0, w, h, screen, r.left, r.top, SRCCOPY);

        let mut bi = BITMAPINFO::default();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h; // сверху вниз
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB.0;
        let mut px = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(mem, bmp, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);

        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
        ReleaseDC(HWND::default(), screen);

        if blt.is_err() || lines == 0 {
            return Err("не удалось снять экран".into());
        }
        for c in px.chunks_exact_mut(4) {
            c.swap(0, 2); // BGRA -> RGBA
            c[3] = 255;
        }
        Ok((w as usize, h as usize, px))
    }
}

pub fn save_png(w: usize, h: usize, rgba: &[u8]) -> Result<String, String> {
    let home = std::env::var("USERPROFILE").map_err(|e| e.to_string())?;
    let dir = Path::new(&home).join("Pictures").join("Островок");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("Снимок {}.png", chrono::Local::now().format("%Y-%m-%d %H-%M-%S")));
    image::save_buffer(&path, rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

/// Распознавание текста встроенным в Windows движком — всё локально, без интернета.
pub fn ocr(w: usize, h: usize, rgba: &[u8]) -> Result<String, String> {
    use windows::Graphics::Imaging::{BitmapAlphaMode, BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;
    let e = |e: windows::core::Error| e.to_string();

    let Some(mut img) = image::RgbaImage::from_raw(w as u32, h as u32, rgba.to_vec()) else {
        return Err("повреждённый снимок".into());
    };
    // мелкий текст распознаётся заметно лучше после увеличения
    if w < 1600 && h < 1600 {
        img = image::imageops::resize(&img, (w * 2) as u32, (h * 2) as u32, image::imageops::FilterType::CatmullRom);
    }
    let (iw, ih) = (img.width() as i32, img.height() as i32);
    let mut bgra = img.into_raw();
    for c in bgra.chunks_exact_mut(4) {
        c.swap(0, 2);
        c[3] = 255;
    }

    let writer = DataWriter::new().map_err(e)?;
    writer.WriteBytes(&bgra).map_err(e)?;
    let buffer = writer.DetachBuffer().map_err(e)?;
    let bmp = SoftwareBitmap::CreateCopyWithAlphaFromBuffer(&buffer, BitmapPixelFormat::Bgra8, iw, ih, BitmapAlphaMode::Premultiplied)
        .map_err(e)?;
    let engine = OcrEngine::TryCreateFromUserProfileLanguages().map_err(|_| "в Windows не установлен язык распознавания".to_string())?;
    let result = engine.RecognizeAsync(&bmp).map_err(e)?.get().map_err(e)?;
    let mut lines = Vec::new();
    for line in result.Lines().map_err(e)? {
        lines.push(line.Text().map_err(e)?.to_string());
    }
    Ok(lines.join("\r\n").trim().to_string())
}

/// Ярлыки из меню «Пуск» и с рабочего стола для поиска.
pub fn index_apps() -> Vec<(String, String)> {
    fn walk(dir: &Path, depth: u8, out: &mut Vec<(String, String)>, seen: &mut std::collections::HashSet<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth < 5 {
                    walk(&path, depth + 1, out, seen);
                }
            } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
                let Some(name) = path.file_stem().and_then(|s| s.to_str()) else { continue };
                let low = name.to_lowercase();
                if low.contains("uninstall") || low.contains("удал") {
                    continue;
                }
                if seen.insert(low) {
                    out.push((name.to_string(), path.display().to_string()));
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut roots = Vec::new();
    if let Ok(a) = std::env::var("APPDATA") {
        roots.push(format!(r"{a}\Microsoft\Windows\Start Menu"));
    }
    if let Ok(p) = std::env::var("PROGRAMDATA") {
        roots.push(format!(r"{p}\Microsoft\Windows\Start Menu"));
    }
    if let Ok(u) = std::env::var("USERPROFILE") {
        roots.push(format!(r"{u}\Desktop"));
    }
    for r in roots {
        walk(Path::new(&r), 0, &mut out, &mut seen);
    }
    out
}

/// Простой калькулятор: + - * / скобки и унарный минус.
pub fn eval(expr: &str) -> Option<f64> {
    struct P<'a> {
        s: &'a [u8],
        i: usize,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.i < self.s.len() && self.s[self.i] == b' ' {
                self.i += 1;
            }
        }
        fn expr(&mut self) -> Option<f64> {
            let mut v = self.term()?;
            loop {
                self.ws();
                match self.s.get(self.i) {
                    Some(b'+') => { self.i += 1; v += self.term()?; }
                    Some(b'-') => { self.i += 1; v -= self.term()?; }
                    _ => return Some(v),
                }
            }
        }
        fn term(&mut self) -> Option<f64> {
            let mut v = self.atom()?;
            loop {
                self.ws();
                match self.s.get(self.i) {
                    Some(b'*') => { self.i += 1; v *= self.atom()?; }
                    Some(b'/') => { self.i += 1; v /= self.atom()?; }
                    _ => return Some(v),
                }
            }
        }
        fn atom(&mut self) -> Option<f64> {
            self.ws();
            match self.s.get(self.i)? {
                b'-' => { self.i += 1; Some(-self.atom()?) }
                b'(' => {
                    self.i += 1;
                    let v = self.expr()?;
                    self.ws();
                    if self.s.get(self.i) == Some(&b')') { self.i += 1; Some(v) } else { None }
                }
                _ => {
                    let start = self.i;
                    while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.') {
                        self.i += 1;
                    }
                    std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()
                }
            }
        }
    }
    let clean = expr.replace(',', ".");
    let mut p = P { s: clean.as_bytes(), i: 0 };
    let v = p.expr()?;
    p.ws();
    (p.i == p.s.len() && v.is_finite()).then_some(v)
}

/// Звуковой сигнал системы (для таймера).
pub fn beep() {
    unsafe {
        let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(MB_ICONEXCLAMATION);
    }
}

/// Показатели компьютера для вкладки «Система».
#[derive(Default, Clone)]
pub struct Stats {
    pub cpu: f32,
    pub mem_used: f64,
    pub mem_total: f64,
    pub disk_free: f64,
    pub disk_total: f64,
    /// заряд в процентах и признак питания от сети; None — батареи нет
    pub battery: Option<(u8, bool)>,
    pub uptime: u64,
}

#[derive(Default)]
pub struct CpuMeter {
    prev: Option<(u64, u64)>,
}

impl CpuMeter {
    /// Загрузка процессора считается по разнице с прошлым вызовом.
    pub fn sample(&mut self) -> Stats {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
        use windows::Win32::System::SystemInformation::{GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX};
        use windows::Win32::System::Threading::GetSystemTimes;
        const GB: f64 = 1024.0 * 1024.0 * 1024.0;
        let mut s = Stats::default();
        unsafe {
            let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
            if GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).is_ok() {
                let v = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
                let total = v(kernel) + v(user);
                let busy = total.saturating_sub(v(idle));
                if let Some((pt, pb)) = self.prev {
                    let dt = total.saturating_sub(pt);
                    if dt > 0 {
                        s.cpu = (busy.saturating_sub(pb) as f32 / dt as f32).clamp(0.0, 1.0);
                    }
                }
                self.prev = Some((total, busy));
            }

            let mut mem = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
            if GlobalMemoryStatusEx(&mut mem).is_ok() {
                s.mem_total = mem.ullTotalPhys as f64 / GB;
                s.mem_used = (mem.ullTotalPhys - mem.ullAvailPhys) as f64 / GB;
            }

            let (mut free, mut total) = (0u64, 0u64);
            if GetDiskFreeSpaceExW(w!("C:\\"), Some(&mut free), Some(&mut total), None).is_ok() {
                s.disk_free = free as f64 / GB;
                s.disk_total = total as f64 / GB;
            }

            let mut power = SYSTEM_POWER_STATUS::default();
            if GetSystemPowerStatus(&mut power).is_ok() && power.BatteryFlag & 128 == 0 && power.BatteryLifePercent <= 100 {
                s.battery = Some((power.BatteryLifePercent, power.ACLineStatus == 1));
            }
            s.uptime = GetTickCount64() / 1000;
        }
        s
    }
}

/// Открывает сайт чата компактным окном без вкладок в браузере пользователя (там уже есть его вход в аккаунт).
/// Если браузер по умолчанию не умеет такой режим, сайт открывается обычной вкладкой.
pub fn open_chat_window(url: &str) {
    use winreg::{enums::*, RegKey};
    let exe = (|| -> Option<String> {
        let prog: String = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice")
            .ok()?
            .get_value("ProgId")
            .ok()?;
        let cmd: String = RegKey::predef(HKEY_CLASSES_ROOT).open_subkey(format!(r"{prog}\shell\open\command")).ok()?.get_value("").ok()?;
        let cmd = cmd.trim();
        Some(match cmd.strip_prefix('"') {
            Some(rest) => rest.split('"').next()?.to_string(),
            None => cmd.split(' ').next()?.to_string(),
        })
    })();
    let x = (screen_width() / 2 - 250).max(0);
    let chromium = ["chrome.exe", "msedge.exe", "brave.exe", "vivaldi.exe"];
    match exe {
        Some(e) if chromium.iter().any(|n| e.to_lowercase().ends_with(n)) => {
            let started = std::process::Command::new(&e)
                .arg(format!("--app={url}"))
                .arg("--window-size=500,720")
                .arg(format!("--window-position={x},64"))
                .spawn()
                .is_ok();
            if !started {
                open(url);
            }
        }
        _ => open(url),
    }
}

/// Активное окно принадлежит Островку (в том числе когда печатают во встроенном чате).
pub fn we_are_foreground() -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return false;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        pid == std::process::id()
    }
}

/// Обрезает окно по форме острова (скруглённый прямоугольник, в пикселях окна).
/// Системное размытие фона и щелчки мыши действуют только внутри этой формы.
pub fn set_region(hwnd: isize, left: i32, top: i32, right: i32, bottom: i32, radius: i32) {
    unsafe {
        let rgn = CreateRoundRectRgn(left, top, right + 1, bottom + 1, radius * 2, radius * 2);
        SetWindowRgn(HWND(hwnd as *mut core::ffi::c_void), rgn, true);
    }
}

/// Цвет точки экрана под курсором в виде #RRGGBB.
pub fn pixel_at_cursor() -> Option<String> {
    unsafe {
        let mut p = POINT::default();
        GetCursorPos(&mut p).ok()?;
        let dc = GetDC(HWND::default());
        let c = GetPixel(dc, p.x, p.y).0;
        ReleaseDC(HWND::default(), dc);
        if c == 0xFFFF_FFFF {
            return None;
        }
        Some(format!("#{:02X}{:02X}{:02X}", c & 0xFF, (c >> 8) & 0xFF, (c >> 16) & 0xFF))
    }
}

/// Следит за папкой «Загрузки» и сообщает имя каждого нового готового файла.
pub fn spawn_downloads(report: impl Fn(String) + Send + 'static) {
    std::thread::spawn(move || {
        let Ok(home) = std::env::var("USERPROFILE") else { return };
        let dir = Path::new(&home).join("Downloads");
        let list = || -> std::collections::HashSet<String> {
            std::fs::read_dir(&dir)
                .map(|rd| rd.flatten().filter(|e| e.path().is_file()).filter_map(|e| e.file_name().into_string().ok()).collect())
                .unwrap_or_default()
        };
        let unfinished = |n: &str| {
            let l = n.to_lowercase();
            [".crdownload", ".tmp", ".part", ".download", ".opdownload"].iter().any(|s| l.ends_with(s)) || l.starts_with("~") || l == "desktop.ini"
        };
        let mut seen = list();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(3));
            let now = list();
            for name in now.difference(&seen) {
                if !unfinished(name) {
                    report(name.clone());
                }
            }
            seen = now;
        }
    });
}

/// Нажимает системную клавишу (громкость, звук).
pub fn press_key(vk: u8) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP};
    unsafe {
        keybd_event(vk, 0, KEYBD_EVENT_FLAGS(0), 0);
        keybd_event(vk, 0, KEYEVENTF_KEYUP, 0);
    }
}

/// Блокировка, сон, перезагрузка и выключение — штатными командами Windows.
pub fn power(action: &str) {
    let (exe, args): (&str, &[&str]) = match action {
        "lock" => ("rundll32.exe", &["user32.dll,LockWorkStation"]),
        "sleep" => ("rundll32.exe", &["powrprof.dll,SetSuspendState", "0,1,0"]),
        "restart" => ("shutdown.exe", &["/r", "/t", "0"]),
        "off" => ("shutdown.exe", &["/s", "/t", "0"]),
        _ => return,
    };
    let _ = std::process::Command::new(exe).args(args).spawn();
}

/// Скрывает окно от захвата экрана: остров может «видеть» то, что под ним, и не попадает в собственный снимок.
pub fn exclude_from_capture(hwnd: isize) -> bool {
    unsafe { SetWindowDisplayAffinity(HWND(hwnd as *mut core::ffi::c_void), WDA_EXCLUDEFROMCAPTURE).is_ok() }
}

/// Кусок экрана (пиксели BGRX) — фон для стекла.
pub fn grab_bgrx(x: i32, y: i32, w: i32, h: i32) -> Option<Vec<u8>> {
    if w < 4 || h < 4 {
        return None;
    }
    let mut px = vec![0u8; (w * h * 4) as usize];
    unsafe {
        let screen = GetDC(HWND::default());
        let mem = CreateCompatibleDC(screen);
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp);
        let ok = BitBlt(mem, 0, 0, w, h, screen, x, y, SRCCOPY).is_ok();
        let mut bi = BITMAPINFO::default();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h;
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB.0;
        let lines = GetDIBits(mem, bmp, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp);
        let _ = DeleteDC(mem);
        ReleaseDC(HWND::default(), screen);
        if !ok || lines == 0 {
            return None;
        }
    }
    Some(px)
}

/// Значок файла или ярлыка, как его показывает Проводник: 32×32, RGBA.
pub fn icon_rgba(path: &str) -> Option<(usize, usize, Vec<u8>)> {
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
    const N: i32 = 32;
    unsafe {
        let mut info = SHFILEINFOW::default();
        let ok = SHGetFileInfoW(
            &HSTRING::from(path),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if ok == 0 || info.hIcon.is_invalid() {
            return None;
        }
        let mut bi = BITMAPINFO::default();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = N;
        bi.bmiHeader.biHeight = -N;
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB.0;
        let dc = CreateCompatibleDC(HDC::default());
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, windows::Win32::Foundation::HANDLE::default(), 0);
        let mut out = None;
        if let Ok(bmp) = bmp {
            if !bits.is_null() {
                let old = SelectObject(dc, bmp);
                let drawn = DrawIconEx(dc, 0, 0, info.hIcon, N, N, 0, HBRUSH::default(), DI_NORMAL).is_ok();
                let mut px = std::slice::from_raw_parts(bits as *const u8, (N * N * 4) as usize).to_vec();
                SelectObject(dc, old);
                if drawn {
                    // у старых значков нет прозрачности в самих пикселях: считаем непрозрачным всё закрашенное
                    let has_alpha = px.chunks_exact(4).any(|c| c[3] != 0);
                    for c in px.chunks_exact_mut(4) {
                        c.swap(0, 2);
                        if !has_alpha {
                            c[3] = if c[0] | c[1] | c[2] != 0 { 255 } else { 0 };
                        }
                    }
                    out = Some((N as usize, N as usize, px));
                }
            }
            let _ = DeleteObject(bmp);
        }
        let _ = DeleteDC(dc);
        let _ = DestroyIcon(info.hIcon);
        out
    }
}

/// Скрыть окно от захвата экрана или вернуть его обратно (тогда оно видно на демонстрации и записи).
pub fn set_capture_hidden(hwnd: isize, hide: bool) -> bool {
    unsafe { SetWindowDisplayAffinity(HWND(hwnd as *mut core::ffi::c_void), if hide { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE }).is_ok() }
}

// ---------------------------------------------------------------- удобства Windows

fn endpoint_volume(capture: bool) -> Option<windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume> {
    use windows::Win32::Media::Audio::{eCapture, eCommunications, eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = if capture { enumerator.GetDefaultAudioEndpoint(eCapture, eCommunications) } else { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }.ok()?;
        device.Activate(CLSCTX_ALL, None).ok()
    }
}

/// Громкость системы (0…1) и признак «без звука».
pub fn volume() -> Option<(f32, bool)> {
    let v = endpoint_volume(false)?;
    unsafe { Some((v.GetMasterVolumeLevelScalar().ok()?, v.GetMute().ok()?.as_bool())) }
}

pub fn set_volume(level: f32) {
    if let Some(v) = endpoint_volume(false) {
        unsafe {
            let _ = v.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null());
        }
    }
}

pub fn set_mute(mute: bool) {
    if let Some(v) = endpoint_volume(false) {
        unsafe {
            let _ = v.SetMute(mute, std::ptr::null());
        }
    }
}

/// Выключен ли микрофон; None — микрофона нет.
pub fn mic_muted() -> Option<bool> {
    let v = endpoint_volume(true)?;
    unsafe { Some(v.GetMute().ok()?.as_bool()) }
}

pub fn set_mic_mute(mute: bool) {
    if let Some(v) = endpoint_volume(true) {
        unsafe {
            let _ = v.SetMute(mute, std::ptr::null());
        }
    }
}

/// Не давать компьютеру засыпать и гасить экран, пока включено.
pub fn keep_awake(on: bool) {
    use windows::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED};
    unsafe {
        SetThreadExecutionState(if on { ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED } else { ES_CONTINUOUS });
    }
}

const THEME_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// Светлая ли сейчас тема приложений Windows.
pub fn light_theme() -> Option<bool> {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let v: u32 = RegKey::predef(HKEY_CURRENT_USER).open_subkey(THEME_KEY).ok()?.get_value("AppsUseLightTheme").ok()?;
    Some(v != 0)
}

/// Переключает тёмную и светлую тему Windows (и приложений, и панели задач).
pub fn set_light_theme(light: bool) {
    use winreg::{enums::*, RegKey};
    if let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(THEME_KEY, KEY_SET_VALUE) {
        let v: u32 = light as u32;
        let _ = key.set_value("AppsUseLightTheme", &v);
        let _ = key.set_value("SystemUsesLightTheme", &v);
    }
    unsafe {
        // сообщаем открытым программам, что тема изменилась
        let what: Vec<u16> = "ImmersiveColorSet\0".encode_utf16().collect();
        SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(what.as_ptr() as isize), SMTO_ABORTIFHUNG, 200, None);
    }
}

/// Сколько в корзине: число объектов и размер в байтах.
pub fn recycle_bin() -> (i64, i64) {
    use windows::Win32::UI::Shell::{SHQueryRecycleBinW, SHQUERYRBINFO};
    unsafe {
        let mut info = SHQUERYRBINFO { cbSize: std::mem::size_of::<SHQUERYRBINFO>() as u32, ..Default::default() };
        if SHQueryRecycleBinW(PCWSTR::null(), &mut info).is_ok() {
            let (items, size) = (info.i64NumItems, info.i64Size);
            (items, size)
        } else {
            (0, 0)
        }
    }
}

/// Очищает корзину без вопросов Windows — подтверждение спрашивает сам остров.
pub fn empty_recycle_bin() {
    use windows::Win32::UI::Shell::{SHEmptyRecycleBinW, SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND};
    unsafe {
        let _ = SHEmptyRecycleBinW(HWND::default(), PCWSTR::null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND);
    }
}

/// Активное окно, если оно не наше и не рабочий стол (0 — такого нет).
pub fn foreground_other() -> isize {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return 0;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, Some(&mut pid));
        if pid == std::process::id() {
            return 0;
        }
        let mut buf = [0u16; 64];
        let n = GetClassNameW(fg, &mut buf) as usize;
        let class = String::from_utf16_lossy(&buf[..n]);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd") {
            return 0;
        }
        fg.0 as isize
    }
}

pub fn window_title(hwnd: isize) -> String {
    unsafe {
        let mut buf = [0u16; 160];
        let n = GetWindowTextW(HWND(hwnd as *mut core::ffi::c_void), &mut buf) as usize;
        String::from_utf16_lossy(&buf[..n])
    }
}

pub fn is_topmost(hwnd: isize) -> bool {
    unsafe { GetWindowLongW(HWND(hwnd as *mut core::ffi::c_void), GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 != 0 }
}

/// Закрепляет окно поверх остальных или снимает закрепление.
pub fn set_topmost(hwnd: isize, on: bool) {
    unsafe {
        let _ = SetWindowPos(
            HWND(hwnd as *mut core::ffi::c_void),
            if on { HWND_TOPMOST } else { HWND_NOTOPMOST },
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

// ---------------------------------------------------------------- громкость отдельных программ

/// Программа, которая выводит звук: имя, путь к файлу, громкость 0…1.
#[derive(Clone)]
pub struct AppVol {
    pub name: String,
    pub path: String,
    pub level: f32,
    pub muted: bool,
    pub active: bool,
}

/// Обходит звуковые сеансы устройства вывода: путь к программе, играет ли она сейчас, её регулятор.
fn audio_sessions(mut f: impl FnMut(String, bool, &windows::Win32::Media::Audio::ISimpleAudioVolume)) {
    use windows::core::Interface;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, AudioSessionStateActive, AudioSessionStateExpired, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, ISimpleAudioVolume,
        MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let Ok(enumerator) = CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL) else { return };
        let Ok(device) = enumerator.GetDefaultAudioEndpoint(eRender, eConsole) else { return };
        let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { return };
        let Ok(sessions) = manager.GetSessionEnumerator() else { return };
        for i in 0..sessions.GetCount().unwrap_or(0) {
            let Ok(session) = sessions.GetSession(i) else { continue };
            let state = session.GetState().ok();
            if state == Some(AudioSessionStateExpired) {
                continue;
            }
            let Ok(pid) = session.cast::<IAudioSessionControl2>().and_then(|s| s.GetProcessId()) else { continue };
            if pid == 0 {
                continue; // системные звуки
            }
            let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { continue };
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
            let _ = windows::Win32::Foundation::CloseHandle(process);
            if !ok {
                continue;
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            if let Ok(vol) = session.cast::<ISimpleAudioVolume>() {
                f(path, state == Some(AudioSessionStateActive), &vol);
            }
        }
    }
}

/// Программы со звуком: сначала те, что играют прямо сейчас.
pub fn app_volumes() -> Vec<AppVol> {
    let mut out: Vec<AppVol> = Vec::new();
    audio_sessions(|path, active, vol| unsafe {
        if let Some(seen) = out.iter_mut().find(|a| a.path.eq_ignore_ascii_case(&path)) {
            seen.active |= active;
            return;
        }
        let file = path.rsplit('\\').next().unwrap_or(&path);
        let stem = file.rsplit_once('.').map_or(file, |s| s.0).to_string();
        let name = if stem.chars().count() <= 4 {
            stem.to_uppercase()
        } else {
            let mut c = stem.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        };
        out.push(AppVol { name, path, level: vol.GetMasterVolume().unwrap_or(1.0), muted: vol.GetMute().map(|m| m.as_bool()).unwrap_or(false), active });
    });
    out.sort_by(|a, b| b.active.cmp(&a.active).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

pub fn set_app_volume(path: &str, level: f32) {
    audio_sessions(|p, _, vol| unsafe {
        if p.eq_ignore_ascii_case(path) {
            let _ = vol.SetMasterVolume(level.clamp(0.0, 1.0), std::ptr::null());
        }
    });
}

pub fn set_app_mute(path: &str, mute: bool) {
    audio_sessions(|p, _, vol| unsafe {
        if p.eq_ignore_ascii_case(path) {
            let _ = vol.SetMute(mute, std::ptr::null());
        }
    });
}

/// Какие программы занимают больше всего оперативной памяти. Процессы одной программы складываются
/// (у браузера их десятки). Оценка приблизительная: общие страницы памяти считаются у каждого процесса.
pub fn top_memory(n: usize) -> Vec<(String, u64)> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let mut sum: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return Vec::new() };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
                let name = String::from_utf16_lossy(&e.szExeFile[..len]);
                if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, e.th32ProcessID) {
                    let mut pmc = PROCESS_MEMORY_COUNTERS::default();
                    if GetProcessMemoryInfo(h, &mut pmc, std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32).is_ok() {
                        *sum.entry(name).or_default() += pmc.WorkingSetSize as u64;
                    }
                    let _ = CloseHandle(h);
                }
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    let mut list: Vec<(String, u64)> = sum
        .into_iter()
        .map(|(name, bytes)| {
            let stem = name.strip_suffix(".exe").or_else(|| name.strip_suffix(".EXE")).unwrap_or(&name);
            let mut c = stem.chars();
            (c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default(), bytes)
        })
        .collect();
    list.sort_by(|a, b| b.1.cmp(&a.1));
    list.truncate(n);
    list
}

// ---------------------------------------------------------------- сеть и завершение программ

/// Сколько байт принято и отправлено настоящими сетевыми адаптерами с момента включения компьютера.
pub fn net_bytes() -> Option<(u64, u64)> {
    use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut table).is_err() || table.is_null() {
            return None;
        }
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        let (mut rx, mut tx) = (0u64, 0u64);
        for r in rows {
            // только «железные» адаптеры: виртуальные и фильтры повторяют те же байты
            if r.InterfaceAndOperStatusFlags._bitfield & 1 != 0 {
                rx += r.InOctets;
                tx += r.OutOctets;
            }
        }
        FreeMibTable(table as *const core::ffi::c_void);
        Some((rx, tx))
    }
}

/// Можно ли предлагать закрыть программу: системные процессы и сам остров трогать нельзя.
pub fn can_kill(name: &str) -> bool {
    const KEEP: [&str; 20] = [
        "system", "registry", "memory compression", "memcompression", "smss", "csrss", "wininit", "winlogon", "services", "lsass", "svchost", "dwm", "explorer", "fontdrvhost",
        "sihost", "ctfmon", "msmpeng", "securityhealthservice", "ostrovok", "audiodg",
    ];
    let low = name.to_lowercase();
    !low.is_empty() && !KEEP.contains(&low.as_str())
}

/// Завершает все процессы программы по имени файла (без «.exe»). Возвращает, сколько завершено.
pub fn kill_by_name(name: &str) -> usize {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    if !can_kill(name) {
        return 0;
    }
    let want = format!("{}.exe", name.to_lowercase());
    let mut done = 0;
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return 0 };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
                if String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase() == want && e.th32ProcessID != std::process::id() {
                    if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, e.th32ProcessID) {
                        if TerminateProcess(h, 1).is_ok() {
                            done += 1;
                        }
                        let _ = CloseHandle(h);
                    }
                }
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    done
}

// ---------------------------------------------------------------- наблюдение за громкостью и раскладкой

/// Что сейчас с общей громкостью и на каком языке печатает активное окно.
#[derive(Clone, Copy, Default, PartialEq)]
pub struct Watch {
    pub vol: f32,
    pub muted: bool,
    /// код языка раскладки (0x419 — русская, 0x409 — английская), 0 — неизвестно
    pub layout: u16,
    /// удалось ли прочитать громкость
    pub ok: bool,
}

/// Следит за громкостью и раскладкой в стороне от интерфейса; wake зовётся при каждом изменении.
pub fn spawn_watch(wake: impl Fn() + Send + 'static) -> Arc<std::sync::Mutex<Watch>> {
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyboardLayout;
    let shared = Arc::new(std::sync::Mutex::new(Watch::default()));
    let out = shared.clone();
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let mut endpoint = None;
        let mut tick = 0u32;
        loop {
            // устройство вывода могли сменить — раз в несколько секунд берём его заново
            if endpoint.is_none() || tick % 40 == 0 {
                endpoint = endpoint_volume(false);
            }
            tick = tick.wrapping_add(1);
            let mut w = Watch::default();
            if let Some(v) = &endpoint {
                if let (Ok(level), Ok(mute)) = (v.GetMasterVolumeLevelScalar(), v.GetMute()) {
                    w.vol = level;
                    w.muted = mute.as_bool();
                    w.ok = true;
                }
            }
            if !w.ok {
                endpoint = None;
            }
            let tid = GetWindowThreadProcessId(GetForegroundWindow(), None);
            w.layout = (GetKeyboardLayout(tid).0 as usize & 0xFFFF) as u16;
            let changed = {
                let mut s = shared.lock().unwrap();
                let c = *s != w;
                *s = w;
                c
            };
            if changed {
                wake();
            }
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
    });
    out
}

// ---------------------------------------------------------------- перетаскивание файла из острова

/// Начинает перетаскивание файла в другую программу — как из Проводника.
/// Возвращается, когда файл бросили или перетаскивание отменили.
pub fn drag_file(path: &str) -> bool {
    use windows::Win32::System::Com::IDataObject;
    use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_LINK};
    use windows::Win32::UI::Shell::{BHID_DataObject, IShellItem, SHCreateItemFromParsingName, SHDoDragDrop};
    unsafe {
        let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(path), None) else { return false };
        let Ok(data) = item.BindToHandler::<_, IDataObject>(None, &BHID_DataObject) else { return false };
        SHDoDragDrop(None, &data, None, DROPEFFECT_COPY | DROPEFFECT_LINK).is_ok()
    }
}
