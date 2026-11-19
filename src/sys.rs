//! Обёртки над Windows: курсор, горячая клавиша, буфер, снимок экрана, распознавание текста, запуск программ.

use std::path::{Path, PathBuf};
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

/// Решает по меткам буфера, секрет ли это (менеджеры паролей и т. п.).
/// `exclude` — есть формат ExcludeClipboardContentFromMonitorProcessing;
/// `history`/`cloud` — значения CanIncludeInClipboardHistory / CanUploadToCloudClipboard, если формат есть.
pub fn clip_is_private(exclude: bool, history: Option<u32>, cloud: Option<u32>) -> bool {
    exclude || history == Some(0) || cloud == Some(0)
}

/// Some(true), если содержимое буфера помечено как приватное; None — буфер занят, спросить позже.
pub fn clipboard_private() -> Option<bool> {
    use windows::Win32::Foundation::{HANDLE, HGLOBAL};
    use windows::Win32::System::DataExchange::*;
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
    unsafe {
        let fmt = |name: PCWSTR| RegisterClipboardFormatW(name);
        let excl = fmt(w!("ExcludeClipboardContentFromMonitorProcessing"));
        let hist = fmt(w!("CanIncludeInClipboardHistory"));
        let cloud = fmt(w!("CanUploadToCloudClipboard"));
        let has = |f: u32| f != 0 && IsClipboardFormatAvailable(f).is_ok();
        let exclude = has(excl);
        if exclude {
            return Some(true);
        }
        let (hh, hc) = (has(hist), has(cloud));
        if !hh && !hc {
            return Some(false);
        }
        // формат есть, а прочитать не удалось — перестраховываемся
        let read = |f: u32| -> Option<u32> {
            let h: HANDLE = GetClipboardData(f).ok()?;
            let g = HGLOBAL(h.0);
            if GlobalSize(g) < 4 {
                return Some(0);
            }
            let p = GlobalLock(g) as *const u8;
            if p.is_null() {
                return Some(0);
            }
            let v = u32::from_le_bytes([*p, *p.add(1), *p.add(2), *p.add(3)]);
            let _ = GlobalUnlock(g);
            Some(v)
        };
        // без ожидания в потоке интерфейса: занят — вызывающий повторит на следующем кадре
        if OpenClipboard(None).is_err() {
            return None;
        }
        let h = if hh { Some(read(hist).unwrap_or(0)) } else { None };
        let c = if hc { Some(read(cloud).unwrap_or(0)) } else { None };
        let _ = CloseClipboard();
        Some(clip_is_private(false, h, c))
    }
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

/// Есть ли полноэкранное приложение на любом мониторе — для осторожных действий
/// (перезапуск после обновления, создание окна при запуске).
pub fn fullscreen_app_active() -> bool {
    fullscreen_on(None)
}

/// Нужно ли спрятать остров, стоящий в точке (x, y) экрана: активное чужое окно на том же мониторе
/// занимает его целиком или это полноэкранная игра/презентация. Полноэкранное окно на другом мониторе не мешает.
pub fn fullscreen_app_active_at(x: i32, y: i32) -> bool {
    fullscreen_on(Some(monitor_at(x, y)))
}

fn fullscreen_on(island_mon: Option<[i32; 4]>) -> bool {
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
        let win_mon = [m.left, m.top, m.right, m.bottom];
        let game = matches!(SHQueryUserNotificationState(), Ok(st) if st == QUNS_RUNNING_D3D_FULL_SCREEN || st == QUNS_PRESENTATION_MODE);
        crate::place::hides_island([r.left, r.top, r.right, r.bottom], win_mon, island_mon.unwrap_or(win_mon), game)
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

/// Быстрый проход: только имена и пути .lnk, цели ярлыков не читаем.
pub fn scan_lnks() -> Vec<(String, String)> {
    let mut roots = Vec::new();
    if let Ok(a) = std::env::var("APPDATA") {
        roots.push(PathBuf::from(format!(r"{a}\Microsoft\Windows\Start Menu")));
    }
    if let Ok(p) = std::env::var("PROGRAMDATA") {
        roots.push(PathBuf::from(format!(r"{p}\Microsoft\Windows\Start Menu")));
    }
    if let Ok(u) = std::env::var("USERPROFILE") {
        roots.push(PathBuf::from(format!(r"{u}\Desktop")));
    }
    scan_lnks_in(&roots)
}

/// Ярлыки .lnk в папках (до 5 уровней), без деинсталляторов и повторов одного пути.
pub fn scan_lnks_in(roots: &[PathBuf]) -> Vec<(String, String)> {
    fn walk(dir: &Path, depth: u8, out: &mut Vec<(String, String)>, seen: &mut std::collections::HashSet<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() || ft.is_symlink() {
                // ссылки и точки повторной обработки (junction) не обходим — защита от петель
                if ft.is_symlink() || is_reparse(&entry) {
                    continue;
                }
                if depth < 5 {
                    walk(&path, depth + 1, out, seen);
                }
            } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
                let Some(name) = path.file_stem().and_then(|s| s.to_str()) else { continue };
                if is_uninstaller(name) {
                    continue;
                }
                let lnk = path.display().to_string();
                if seen.insert(lnk.to_lowercase()) {
                    out.push((name.to_string(), lnk));
                }
            }
        }
    }
    fn is_reparse(entry: &std::fs::DirEntry) -> bool {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        entry.metadata().map_or(true, |m| m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
    }
    let (mut out, mut seen) = (Vec::new(), std::collections::HashSet::new());
    for r in roots {
        walk(r, 0, &mut out, &mut seen);
    }
    out
}

/// Медленный проход: читает цели ярлыков и убирает повторы (имя, цель).
pub fn resolve_apps(raw: Vec<(String, String)>) -> Vec<(String, String)> {
    let full = raw
        .into_iter()
        .map(|(n, lnk)| {
            let target = lnk_target(&lnk).filter(|t| !t.is_empty()).unwrap_or_else(|| lnk.clone());
            (n, lnk, target)
        })
        .collect();
    dedup_apps(full)
}

/// Ярлык деинсталлятора — в поиск не берём.
pub fn is_uninstaller(name: &str) -> bool {
    let low = name.to_lowercase();
    low.contains("uninstall") || low.contains("деинстал") || low.starts_with("удалить") || low.starts_with("удаление") || low.contains(" удалить") || low.contains(" удаление")
}

/// Убирает повторы по (имя, цель) без учёта регистра; первый побеждает.
/// Вход: (имя, путь .lnk, цель), выход: (имя, путь .lnk).
pub fn dedup_apps(list: Vec<(String, String, String)>) -> Vec<(String, String)> {
    let mut seen = std::collections::HashSet::new();
    list.into_iter().filter(|(n, _, t)| seen.insert((n.to_lowercase(), t.to_lowercase()))).map(|(n, l, _)| (n, l)).collect()
}

/// Таб и неразрывные пробелы (U+00A0, U+202F) — обычный пробел.
fn spaces(s: &str) -> String {
    s.chars().map(|c| if matches!(c, '\t' | '\u{A0}' | '\u{202F}') { ' ' } else { c }).collect()
}

/// «сколько процентов A от B» / «A от B в процентах» → (A, B) как строки.
fn pct_question(s: &str) -> Option<(String, String)> {
    let lc = spaces(s).to_lowercase();
    let lc = lc.trim().trim_end_matches('?').trim_end();
    let body = lc.strip_prefix("сколько процентов ").or_else(|| lc.strip_suffix(" в процентах"))?;
    let (a, b) = body.split_once(" от ")?;
    let (a, b) = (a.trim(), b.trim());
    (!a.is_empty() && !b.is_empty()).then(|| (a.to_string(), b.to_string()))
}

/// Вопрос «сколько процентов A от B» — ответ показывать со знаком «%».
pub fn asks_percent(s: &str) -> bool {
    pct_question(s).is_some()
}

/// Приводит запрос к виду калькулятора: «50% от 300» → «50%*300», «15 процентов от 200» → «15%*200»,
/// «сколько процентов 30 от 200» → «(30)/(200)*100». Таб и неразрывные пробелы — пробел.
pub fn pct_of(s: &str) -> String {
    if let Some((a, b)) = pct_question(s) {
        return format!("({a})/({b})*100");
    }
    let lc = spaces(s).to_lowercase().replace(" процентов", "%").replace(" процента", "%").replace("%от", "% от");
    match lc.find(" от") {
        Some(pos) if lc[..pos].trim_end().ends_with('%') => format!("{}*{}", &lc[..pos], &lc[pos + " от".len()..]),
        _ if lc.contains('%') => lc,
        _ => spaces(s),
    }
}

/// Простой калькулятор: проценты (200+10% = 220, 50% от 300 = 150), + - * / ^ (степень, правоассоциативна), скобки и унарный минус.
/// Приоритет: -2^2 = -4. Глубина вложенности ограничена (200), длина входа — 1000 символов.
pub fn eval(expr: &str) -> Option<f64> {
    const MAX_DEPTH: usize = 200;
    struct P<'a> {
        s: &'a [u8],
        i: usize,
        depth: usize,
        pct: bool,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.i < self.s.len() && self.s[self.i] == b' ' {
                self.i += 1;
            }
        }
        fn enter(&mut self) -> Option<()> {
            self.depth += 1;
            (self.depth <= MAX_DEPTH).then_some(())
        }
        fn expr(&mut self) -> Option<f64> {
            let mut v = self.term()?;
            loop {
                self.ws();
                match self.s.get(self.i) {
                    Some(b'+') => { self.i += 1; let r = self.term()?; v += if self.pct { v * r } else { r }; }
                    Some(b'-') => { self.i += 1; let r = self.term()?; v -= if self.pct { v * r } else { r }; }
                    _ => return Some(v),
                }
            }
        }
        fn term(&mut self) -> Option<f64> {
            let mut v = self.unary()?;
            loop {
                self.ws();
                match self.s.get(self.i) {
                    Some(b'*') => { self.i += 1; v *= self.unary()?; self.pct = false; }
                    Some(b'/') => { self.i += 1; v /= self.unary()?; self.pct = false; }
                    _ => return Some(v),
                }
            }
        }
        fn unary(&mut self) -> Option<f64> {
            self.ws();
            if self.s.get(self.i) == Some(&b'-') {
                self.i += 1;
                self.enter()?;
                let v = self.unary()?;
                self.depth -= 1;
                Some(-v)
            } else {
                self.power()
            }
        }
        fn power(&mut self) -> Option<f64> {
            self.pct = false;
            let mut base = self.atom()?;
            self.ws();
            if self.s.get(self.i) == Some(&b'%') {
                self.i += 1;
                base /= 100.0;
                self.pct = true;
                self.ws();
            }
            if self.s.get(self.i) != Some(&b'^') {
                return Some(base);
            }
            self.i += 1;
            self.enter()?;
            let exp = self.unary()?;
            self.depth -= 1;
            self.pct = false;
            let v = base.powf(exp);
            v.is_finite().then_some(v)
        }
        fn atom(&mut self) -> Option<f64> {
            self.ws();
            match self.s.get(self.i)? {
                b'(' => {
                    self.i += 1;
                    self.enter()?;
                    let v = self.expr()?;
                    self.depth -= 1;
                    self.pct = false;
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
    if expr.chars().count() > 1000 {
        return None;
    }
    let clean = pct_of(expr).replace(',', ".");
    let mut p = P { s: clean.as_bytes(), i: 0, depth: 0, pct: false };
    let v = p.expr()?;
    p.ws();
    (p.i == p.s.len() && v.is_finite()).then_some(v)
}

#[cfg(test)]
mod pct_tests {
    use super::eval;
    fn ok(s: &str, want: f64) {
        let v = eval(s).unwrap_or_else(|| panic!("{s} -> None"));
        assert!((v - want).abs() < 1e-9, "{s} = {v}, want {want}");
    }
    #[test]
    fn percents() {
        ok("200+10%", 220.0);
        ok("200-10%", 180.0);
        ok("200*10%", 20.0);
        ok("200/10%", 2000.0);
        ok("15%", 0.15);
        ok("50% от 300", 150.0);
        ok("50 % ОТ 300", 150.0);
        ok("(200+10)%", 2.1);
        ok("2^10%", 2f64.powf(0.1));
        ok("-10%", -0.1);
        ok("200+-10%", 180.0);
        ok("200+10%*2", 200.2);
        ok("100+(10%)", 100.1);
        ok("сколько процентов 30 от 200", 15.0);
        ok("30 от 200 в процентах", 15.0);
        ok("Сколько процентов 30 от 200?", 15.0);
        assert_eq!(eval("сколько процентов 30 от 0"), None);
        assert!(super::asks_percent("30 от 200 в процентах"));
        assert!(!super::asks_percent("15% от 200"));
        ok("15%от200", 30.0);
        ok("15% от200", 30.0);
        ok("15 процентов от 200", 30.0);
        ok("15\t+\u{A0}5\u{202F}*2", 25.0);
        ok("50%\u{A0}от\u{202F}300", 150.0);
        for bad in ["от 300", "50 от", "10%%", "%5", "%", "5%(", "сколько процентов 30", "сколько процентов от 200"] {
            assert_eq!(eval(bad), None, "{bad}");
        }
    }
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
    /// прошлое значение — отдаём его, если за тик время не сдвинулось
    last_cpu: f32,
}

/// Доля загрузки процессора между замерами (total, busy). None — нет прошлого замера или время не сдвинулось.
pub fn cpu_frac(prev: Option<(u64, u64)>, cur: (u64, u64)) -> Option<f32> {
    let (pt, pb) = prev?;
    let dt = cur.0.checked_sub(pt).filter(|d| *d > 0)?;
    Some((cur.1.saturating_sub(pb) as f32 / dt as f32).clamp(0.0, 1.0))
}

/// Доля занятого места на диске; None — размер неизвестен или данные несуразные.
pub fn disk_frac(free: f64, total: f64) -> Option<f32> {
    if !(total > 0.0) || !(free >= 0.0) || free > total {
        return None;
    }
    Some((1.0 - free / total) as f32)
}

/// Экспоненциальное сглаживание: чтобы цифры не мигали каждый тик.
pub fn ema(prev: f32, x: f32, alpha: f32) -> f32 {
    if !prev.is_finite() {
        return x;
    }
    prev + (x - prev) * alpha.clamp(0.0, 1.0)
}

/// Скорость сети (приём, отдача) в байтах/с по счётчикам (rx, tx) за dt секунд.
/// Первый замер и слишком короткий тик — None (оставить прошлое значение);
/// сброс счётчика (стал меньше) или скачок больше 10 ГБ/с (появился адаптер, VPN) — тоже None, база перебазируется.
pub fn net_rate(prev: Option<(u64, u64)>, cur: (u64, u64), dt: f32) -> Option<(f32, f32)> {
    const MAX: f32 = 10.0 * 1024.0 * 1024.0 * 1024.0;
    let (prx, ptx) = prev?;
    if !(dt >= 0.2) {
        return None;
    }
    let drx = cur.0.checked_sub(prx)?;
    let dtx = cur.1.checked_sub(ptx)?;
    let r = (drx as f32 / dt, dtx as f32 / dt);
    (r.0 <= MAX && r.1 <= MAX).then_some(r)
}

#[cfg(test)]
mod meter_tests {
    use super::*;

    #[test]
    fn cpu() {
        assert_eq!(cpu_frac(None, (100, 50)), None);
        assert_eq!(cpu_frac(Some((100, 50)), (100, 60)), None, "dt=0");
        assert_eq!(cpu_frac(Some((100, 50)), (90, 60)), None, "время назад");
        assert_eq!(cpu_frac(Some((100, 50)), (200, 100)), Some(0.5));
        assert_eq!(cpu_frac(Some((100, 50)), (200, 500)), Some(1.0), "clamp сверху");
        assert_eq!(cpu_frac(Some((100, 50)), (200, 10)), Some(0.0), "clamp снизу");
    }

    #[test]
    fn disk() {
        assert_eq!(disk_frac(0.0, 0.0), None);
        assert_eq!(disk_frac(5.0, 10.0), Some(0.5));
        assert_eq!(disk_frac(11.0, 10.0), None);
        assert_eq!(disk_frac(0.0, 10.0), Some(1.0));
    }

    #[test]
    fn net() {
        assert_eq!(net_rate(None, (1000, 1000), 1.0), None, "первый замер");
        assert_eq!(net_rate(Some((0, 0)), (1000, 1000), 0.0), None, "dt=0");
        assert_eq!(net_rate(Some((0, 0)), (1000, 1000), 0.1), None, "слишком короткий тик");
        assert_eq!(net_rate(Some((5000, 0)), (1000, 1000), 1.0), None, "сброс счётчика");
        assert_eq!(net_rate(Some((0, 0)), (50 << 30, 0), 1.0), None, "скачок от нового адаптера");
        assert_eq!(net_rate(Some((0, 0)), (2048, 1024), 2.0), Some((1024.0, 512.0)));
    }

    #[test]
    fn smoothing() {
        assert_eq!(ema(0.0, 10.0, 0.4), 4.0);
        assert_eq!(ema(10.0, 10.0, 0.4), 10.0);
        assert_eq!(ema(f32::NAN, 3.0, 0.4), 3.0);
        assert_eq!(ema(0.0, 10.0, 1.0), 10.0);
    }
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
                match cpu_frac(self.prev, (total, busy)) {
                    Some(f) => self.last_cpu = f,
                    None => {}
                }
                s.cpu = self.last_cpu;
                self.prev = Some((total, busy));
            }

            let mut mem = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
            if GlobalMemoryStatusEx(&mut mem).is_ok() {
                s.mem_total = mem.ullTotalPhys as f64 / GB;
                s.mem_used = mem.ullTotalPhys.saturating_sub(mem.ullAvailPhys) as f64 / GB;
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
#[allow(dead_code)] // запасной помощник: сейчас не вызывается
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
#[allow(dead_code)] // запасной помощник: сейчас не вызывается
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

/// Следит за папкой «Загрузки»: сообщает имя каждого нового готового файла, а пока что-то качается —
/// пишет в progress, сколько байт уже скачано и с какой скоростью.
pub fn spawn_downloads(report: impl Fn(String) + Send + 'static, progress: Arc<std::sync::Mutex<Option<(u64, f32)>>>) {
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
        // недокачанные файлы браузеров: по их росту видно ход загрузки
        let partial = || -> u64 {
            std::fs::read_dir(&dir)
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| {
                            let l = e.file_name().to_string_lossy().to_lowercase();
                            [".crdownload", ".part", ".opdownload", ".download"].iter().any(|s| l.ends_with(s))
                        })
                        .filter_map(|e| e.metadata().ok())
                        .map(|m| m.len())
                        .sum()
                })
                .unwrap_or(0)
        };
        let mut seen = list();
        let (mut last, mut quiet, mut tick) = (partial(), 0u32, 0u32);
        loop {
            // пока ничего не качается, папку смотрим раз в три секунды, а не каждую — меньше работы диску во время игр
            let busy = last > 0;
            // в полноэкранной игре — раз в 15 с
            let pause = crate::quiet::poll_every(crate::quiet::Src::Downloads { busy }, crate::quiet::in_game());
            std::thread::sleep(pause);
            let size = partial();
            let dt = pause.as_secs_f32();
            let speed = size.saturating_sub(last) as f32 / dt;
            last = size;
            // загрузка считается идущей, пока файл растёт; три секунды без роста — пауза или конец
            quiet = if speed > 0.0 { 0 } else { quiet + dt as u32 };
            *progress.lock().unwrap() = (size > 0 && quiet < 3).then_some((size, speed));
            tick += 1;
            if busy && tick % 3 != 0 {
                continue;
            }
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
#[allow(dead_code)] // запасной помощник: сейчас не вызывается
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
/// Настоящая папка «Изображения» (её могут перенести в OneDrive или на другой диск).
pub fn pictures_dir() -> std::path::PathBuf {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Pictures, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        if let Ok(p) = SHGetKnownFolderPath(&FOLDERID_Pictures, KF_FLAG_DEFAULT, None) {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const _));
            if !s.is_empty() {
                return std::path::PathBuf::from(s);
            }
        }
    }
    std::env::var_os("USERPROFILE").map(|h| std::path::PathBuf::from(h).join("Pictures")).unwrap_or_else(std::env::temp_dir)
}

/// Показывает файл в Проводнике выделенным.
pub fn reveal(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
}

/// Куда указывает ярлык .lnk (пусто у ярлыков без пути, например у некоторых магазинных программ).
fn lnk_target(path: &str) -> Option<String> {
    use windows::core::Interface;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, STGM_READ};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    thread_local! {
        // COM на каждом потоке включаем один раз, а не на каждый ярлык
        static COM: () = unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        };
    }
    COM.with(|_| {});
    unsafe {
        let sl: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        sl.cast::<IPersistFile>().ok()?.Load(&HSTRING::from(path), STGM_READ).ok()?;
        let mut buf = [0u16; 520];
        sl.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        let target = String::from_utf16_lossy(&buf[..end]);
        (!target.is_empty() && std::path::Path::new(&target).exists()).then_some(target)
    }
}

pub fn icon_rgba(path: &str) -> Option<(usize, usize, Vec<u8>)> {
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON};
    const N: i32 = 32;
    // у ярлыка значок берём с самой программы — так на нём нет стрелки ярлыка
    let resolved = if path.to_lowercase().ends_with(".lnk") { lnk_target(path) } else { None };
    let path = resolved.as_deref().unwrap_or(path);
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

/// Пиковый уровень устройства вывода по умолчанию (IAudioMeterInformation) — для эквалайзера в пилюле.
/// Звук не захватывается: Windows отдаёт только пики. Объект живёт в потоке интерфейса
/// и пересоздаётся раз в 3 секунды, чтобы переживать смену устройства вывода.
pub struct Meter {
    meter: windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation,
    born: std::time::Instant,
}

impl Meter {
    pub fn open() -> Option<Self> {
        use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
            let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
            let meter = device.Activate(CLSCTX_ALL, None).ok()?;
            Some(Self { meter, born: std::time::Instant::now() })
        }
    }

    /// Пора пересоздать (устройство вывода могли сменить).
    pub fn stale(&self) -> bool {
        self.born.elapsed() > std::time::Duration::from_secs(3)
    }

    /// Общий пик и пики левого/правого каналов, 0…1 (выключенный звук учитывает вызывающий).
    pub fn peaks(&self) -> Option<(f32, [f32; 2])> {
        unsafe {
            let peak = sane_level(self.meter.GetPeakValue().ok()?).unwrap_or(0.0);
            let n = (self.meter.GetMeteringChannelCount().unwrap_or(0) as usize).min(32);
            let mut ch = [0.0f32; 32];
            let lr = if n >= 1 && self.meter.GetChannelsPeakValues(&mut ch[..n]).is_ok() {
                let l = sane_level(ch[0]).unwrap_or(0.0);
                let r = if n >= 2 { sane_level(ch[1]).unwrap_or(0.0) } else { l };
                [l, r]
            } else {
                [peak, peak]
            };
            Some((peak, lr))
        }
    }
}

/// Громкость системы (0…1) и признак «без звука».
pub fn volume() -> Option<(f32, bool)> {
    let v = endpoint_volume(false)?;
    unsafe { Some((v.GetMasterVolumeLevelScalar().ok()?, v.GetMute().ok()?.as_bool())) }
}

/// Громкость в 0..1; NaN и бесконечность отбрасываются.
pub fn sane_level(v: f32) -> Option<f32> {
    v.is_finite().then(|| v.clamp(0.0, 1.0))
}

pub fn set_volume(level: f32) {
    let Some(level) = sane_level(level) else { return };
    if let Some(v) = endpoint_volume(false) {
        unsafe {
            let _ = v.SetMasterVolumeLevelScalar(level, std::ptr::null());
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
            let mut buf = vec![0u16; 512];
            let mut len = buf.len() as u32;
            let r = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len);
            let mut ok = r.is_ok();
            if matches!(&r, Err(e) if e.code() == windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER.to_hresult()) {
                // длинный путь (\\?\…) не влез — повторяем с наибольшим буфером
                buf = vec![0u16; 32768];
                len = buf.len() as u32;
                ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
            }
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

/// Показываемое имя программы по пути: короткие (vlc) — заглавными, прочие — с заглавной буквы.
pub fn app_display_name(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let stem = file.rsplit_once('.').map_or(file, |s| s.0);
    if stem.chars().count() <= 4 {
        stem.to_uppercase()
    } else {
        let mut c = stem.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    }
}

/// Склеивает ещё одну звуковую сессию той же программы: уровень — у играющей сессии (иначе наибольший),
/// заглушена программа, только если заглушены все её сессии.
fn merge_app(seen: &mut AppVol, other: AppVol) {
    if other.active && !seen.active {
        seen.level = other.level;
    } else if other.active == seen.active {
        seen.level = seen.level.max(other.level);
    }
    seen.muted &= other.muted;
    seen.active |= other.active;
}

/// Одинаковые имена у разных программ различает папкой: «Game (One)», «Game (Two)».
fn disambiguate(apps: &mut [AppVol]) {
    let folder = |p: &str| -> String {
        let mut parts = p.rsplit(['\\', '/']);
        parts.next();
        parts.next().unwrap_or("").to_string()
    };
    let clash: Vec<bool> = apps
        .iter()
        .map(|a| apps.iter().any(|b| b.name.to_lowercase() == a.name.to_lowercase() && !b.path.eq_ignore_ascii_case(&a.path)))
        .collect();
    for (a, c) in apps.iter_mut().zip(clash) {
        let f = folder(&a.path);
        if c && !f.is_empty() {
            a.name = format!("{} ({f})", a.name);
        }
    }
}

/// Программы со звуком: сначала те, что играют прямо сейчас. Сессии одного exe склеены.
pub fn app_volumes() -> Vec<AppVol> {
    let mut out: Vec<AppVol> = Vec::new();
    audio_sessions(|path, active, vol| unsafe {
        let app = AppVol {
            name: app_display_name(&path),
            level: vol.GetMasterVolume().unwrap_or(1.0),
            muted: vol.GetMute().map(|m| m.as_bool()).unwrap_or(false),
            active,
            path,
        };
        match out.iter_mut().find(|a| a.path.eq_ignore_ascii_case(&app.path)) {
            Some(seen) => merge_app(seen, app),
            None => out.push(app),
        }
    });
    disambiguate(&mut out);
    out.sort_by(|a, b| b.active.cmp(&a.active).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

/// UNC-путь к сетевой папке (\\srv\x, \\?\UNC\srv\x); \\?\C:\ и \\.\ — локальные.
pub fn unc_path(p: &str) -> bool {
    if let Some(rest) = p.strip_prefix(r"\\?\") {
        return rest.get(..4).is_some_and(|s| s.eq_ignore_ascii_case(r"UNC\"));
    }
    p.starts_with(r"\\") && !p.starts_with(r"\\.\")
}

/// Корень диска пути: "d:\x" → "D:\".
pub fn drive_root(p: &str) -> Option<String> {
    let p = p.strip_prefix(r"\\?\").unwrap_or(p);
    let mut c = p.chars();
    match (c.next(), c.next()) {
        (Some(d), Some(':')) if d.is_ascii_alphabetic() => Some(format!("{}:\\", d.to_ascii_uppercase())),
        _ => None,
    }
}

/// Сетевой ли путь (UNC или подключённый сетевой диск). Может тормозить — звать только в фоновом потоке.
pub fn is_remote_path(p: &str) -> bool {
    if unc_path(p) {
        return true;
    }
    let Some(root) = drive_root(p) else { return false };
    let wide: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
    const DRIVE_REMOTE: u32 = 4;
    unsafe { windows::Win32::Storage::FileSystem::GetDriveTypeW(windows::core::PCWSTR(wide.as_ptr())) == DRIVE_REMOTE }
}

pub fn set_app_volume(path: &str, level: f32) {
    let Some(level) = sane_level(level) else { return };
    audio_sessions(|p, _, vol| unsafe {
        if p.eq_ignore_ascii_case(path) {
            let _ = vol.SetMasterVolume(level, std::ptr::null());
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
    let mut list: Vec<(String, u64)> = sum.into_iter().map(|(name, bytes)| (pretty_name(&name), bytes)).collect();
    list.sort_by(|a, b| b.1.cmp(&a.1));
    list.truncate(n);
    list
}

/// «chrome.exe» → «Chrome»: так программа называется в списках монитора.
pub fn pretty_name(file: &str) -> String {
    let stem = file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file);
    let mut c = stem.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

/// Суммарное процессорное время (ядро + пользователь, в 100 нс) каждой программы, сложенное по имени.
pub fn proc_times() -> std::collections::HashMap<String, u64> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    use windows::Win32::System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    let ft = |f: FILETIME| (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64;
    let mut sum = std::collections::HashMap::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return sum };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
                if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, e.th32ProcessID) {
                    let (mut a, mut b, mut k, mut u) = Default::default();
                    if GetProcessTimes(h, &mut a, &mut b, &mut k, &mut u).is_ok() {
                        *sum.entry(pretty_name(&String::from_utf16_lossy(&e.szExeFile[..len]))).or_default() += ft(k) + ft(u);
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
    sum
}

/// Кто больше всех грузил процессор между двумя снимками proc_times: доля всей машины (0..1), по убыванию.
/// Программы, появившиеся только во втором снимке или с уменьшившимся временем (перезапуск), пропускаются.
pub fn cpu_top(prev: &std::collections::HashMap<String, u64>, cur: &std::collections::HashMap<String, u64>, elapsed_100ns: u64, cores: usize, n: usize) -> Vec<(String, f32)> {
    if elapsed_100ns == 0 {
        return Vec::new();
    }
    let whole = elapsed_100ns as f64 * cores.max(1) as f64;
    let mut list: Vec<(String, f32)> = cur
        .iter()
        .filter_map(|(name, t)| {
            let d = t.checked_sub(*prev.get(name)?)?;
            (d > 0).then(|| (name.clone(), (d as f64 / whole).min(1.0) as f32))
        })
        .collect();
    list.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
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
    /// активное окно и его прямоугольник: при смене (выход из игры, Esc в видео) интерфейс просыпается сразу
    pub fg: isize,
    pub fg_rect: [i32; 4],
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
            let fg = GetForegroundWindow();
            w.fg = fg.0 as isize;
            let mut r = windows::Win32::Foundation::RECT::default();
            if GetWindowRect(fg, &mut r).is_ok() {
                w.fg_rect = [r.left, r.top, r.right, r.bottom];
            }
            let tid = GetWindowThreadProcessId(fg, None);
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
            std::thread::sleep(crate::quiet::poll_every(crate::quiet::Src::Watch, crate::quiet::in_game()));
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

#[cfg(test)]
mod level_tests {
    use super::*;
    #[test]
    fn sane_levels() {
        assert_eq!(sane_level(f32::NAN), None);
        assert_eq!(sane_level(f32::INFINITY), None);
        assert_eq!(sane_level(1.5), Some(1.0));
        assert_eq!(sane_level(-0.2), Some(0.0));
        assert_eq!(sane_level(0.4), Some(0.4));
    }
}

#[cfg(test)]
mod cpu_top_tests {
    use super::*;
    use std::collections::HashMap;

    fn m(v: &[(&str, u64)]) -> HashMap<String, u64> {
        v.iter().map(|(k, t)| (k.to_string(), *t)).collect()
    }

    #[test]
    fn shares_of_whole_machine_sorted() {
        // 1 с = 10_000_000 по 100 нс, 4 ядра → вся машина = 40_000_000
        let prev = m(&[("Chrome", 1_000), ("Game", 0), ("Idle", 5)]);
        let cur = m(&[("Chrome", 10_001_000), ("Game", 20_000_000), ("Idle", 5), ("New", 4_000_000)]);
        let top = cpu_top(&prev, &cur, 10_000_000, 4, 2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].0, "Game");
        assert!((top[0].1 - 0.5).abs() < 1e-4);
        assert_eq!(top[1].0, "Chrome");
        assert!((top[1].1 - 0.25).abs() < 1e-4);
    }

    #[test]
    fn skips_new_shrunk_idle_and_bad_input() {
        let prev = m(&[("A", 500), ("B", 100)]);
        let cur = m(&[("A", 100), ("B", 100), ("C", 9_999)]);
        assert!(cpu_top(&prev, &cur, 10_000_000, 4, 5).is_empty());
        assert!(cpu_top(&prev, &prev, 0, 4, 5).is_empty());
        let top = cpu_top(&m(&[("X", 0)]), &m(&[("X", 99_000_000)]), 10_000_000, 0, 5);
        assert_eq!(top[0].1, 1.0, "ноль ядер считаем одним, доля не больше 1");
    }

    #[test]
    fn pretty_names() {
        assert_eq!(pretty_name("chrome.exe"), "Chrome");
        assert_eq!(pretty_name("GAME.EXE"), "GAME");
        assert_eq!(pretty_name("ёлка.exe"), "Ёлка");
        assert_eq!(pretty_name(""), "");
    }
}

#[cfg(test)]
mod apps_index_tests {
    use super::*;

    #[test]
    fn uninstaller_names() {
        assert!(is_uninstaller("Uninstall Foo"));
        assert!(is_uninstaller("Удалить Foo"));
        assert!(is_uninstaller("Удаление программы"));
        assert!(is_uninstaller("Деинсталлятор"));
        assert!(!is_uninstaller("Удалённый рабочий стол"));
        assert!(!is_uninstaller("Firefox"));
    }

    #[test]
    fn dedup_by_name_and_target_first_wins() {
        let s = |a: &str, b: &str, c: &str| (a.to_string(), b.to_string(), c.to_string());
        let v = vec![s("Foo", "a.lnk", "C:\\X.exe"), s("foo", "b.lnk", "c:\\x.exe"), s("Foo", "c.lnk", "C:\\Y.exe"), s("Bar", "d.lnk", "C:\\X.exe")];
        let p = |a: &str, b: &str| (a.to_string(), b.to_string());
        assert_eq!(dedup_apps(v), vec![p("Foo", "a.lnk"), p("Foo", "c.lnk"), p("Bar", "d.lnk")]);
    }
}

#[cfg(test)]
mod scan_tests {
    use super::*;

    #[test]
    fn scan_and_resolve_lnks() {
        let root = std::env::temp_dir().join(format!("ostrovok-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        for f in ["Foo.lnk", "Uninstall Foo.lnk", "sub/Bar.LNK", "readme.txt"] {
            std::fs::write(root.join(f), b"").unwrap();
        }
        let roots = vec![root.clone(), root.clone()];
        let mut raw = scan_lnks_in(&roots);
        raw.sort();
        let names: Vec<&str> = raw.iter().map(|a| a.0.as_str()).collect();
        assert_eq!(names, ["Bar", "Foo"]);
        let res = resolve_apps(raw.clone());
        assert_eq!(res, raw);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_lnks_skips_junction_loop() {
        let root = std::env::temp_dir().join(format!("ostrovok-junc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::write(root.join("real").join("a.lnk"), b"x").unwrap();
        let loop_dir = root.join("real").join("loop");
        let ok = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&loop_dir)
            .arg(&root)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok || !loop_dir.exists() {
            eprintln!("junction не создан, тест пропущен");
            let _ = std::fs::remove_dir_all(&root);
            return;
        }
        let found = scan_lnks_in(&[root.clone()]);
        let _ = std::fs::remove_dir(&loop_dir);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, "a");
    }
}

#[cfg(test)]
mod remote_and_mixer_tests {
    use super::*;

    fn av(name: &str, path: &str, level: f32, muted: bool, active: bool) -> AppVol {
        AppVol { name: name.into(), path: path.into(), level, muted, active }
    }

    #[test]
    fn unc_paths() {
        assert!(unc_path(r"\\srv\x"));
        assert!(unc_path(r"\\?\UNC\srv\x"));
        assert!(!unc_path(r"\\?\C:\a"));
        assert!(!unc_path(r"\\.\C:\a"));
        assert!(!unc_path(r"C:\a"));
        assert!(!unc_path(""));
    }

    #[test]
    fn drive_roots() {
        assert_eq!(drive_root("d:\\x").as_deref(), Some("D:\\"));
        assert_eq!(drive_root("C:").as_deref(), Some("C:\\"));
        assert_eq!(drive_root(r"\\?\e:\a").as_deref(), Some("E:\\"));
        assert_eq!(drive_root(r"\\srv\x"), None);
        assert_eq!(drive_root("x"), None);
        assert_eq!(drive_root(""), None);
        assert_eq!(drive_root("ё:\\"), None);
    }

    #[test]
    fn remote_unc_without_disk_query() {
        assert!(is_remote_path(r"\\srv\share\a.exe"));
        assert!(!is_remote_path(""));
    }

    #[test]
    fn display_names() {
        assert_eq!(app_display_name(r"C:\Program Files\VideoLAN\vlc.exe"), "VLC");
        assert_eq!(app_display_name(r"C:\x\chrome.exe"), "Chrome");
        assert_eq!(app_display_name(""), "");
        assert_eq!(app_display_name(r"C:\x\spotify"), "Spotify");
        assert_eq!(app_display_name(r"C:\Игры\ёлочка.exe"), "Ёлочка");
        assert_eq!(app_display_name(r"C:\x\ёж.exe"), "ЁЖ");
    }

    #[test]
    fn merge_prefers_active_level_and_all_muted() {
        let mut a = av("A", "p", 0.2, true, false);
        merge_app(&mut a, av("A", "p", 0.7, false, true));
        assert_eq!(a.level, 0.7);
        assert!(!a.muted, "заглушена не вся программа");
        assert!(a.active);
        // активная уже есть — неактивная уровень не перебивает
        merge_app(&mut a, av("A", "p", 0.9, true, false));
        assert_eq!(a.level, 0.7);
        // обе неактивные — берём максимум; все заглушены — muted
        let mut b = av("B", "q", 0.3, true, false);
        merge_app(&mut b, av("B", "q", 0.6, true, false));
        assert_eq!(b.level, 0.6);
        assert!(b.muted);
        assert!(!b.active);
    }

    #[test]
    fn disambiguate_same_names() {
        let mut v = vec![
            av("Game", r"C:\One\game.exe", 1.0, false, false),
            av("Game", r"D:\Two\Game.exe", 1.0, false, false),
            av("VLC", r"C:\v\vlc.exe", 1.0, false, false),
        ];
        disambiguate(&mut v);
        assert_eq!(v[0].name, "Game (One)");
        assert_eq!(v[1].name, "Game (Two)");
        assert_eq!(v[2].name, "VLC");
        // один и тот же путь в разном регистре — не трогаем
        let mut w = vec![av("A", r"C:\a\a.exe", 1.0, false, false), av("A", r"c:\A\A.EXE", 1.0, false, false)];
        disambiguate(&mut w);
        assert_eq!(w[0].name, "A");
    }
}

#[cfg(test)]
mod eval_power_tests {
    use super::eval;
    fn close(e: &str, want: f64) {
        let v = eval(e).unwrap_or_else(|| panic!("{e} → None"));
        assert!((v - want).abs() < 1e-9, "{e} = {v}, ждали {want}");
    }
    #[test]
    fn power_basics() {
        close("2^3^2", 512.0);
        close("-2^2", -4.0);
        close("2^-1", 0.5);
        close("(-2)^2", 4.0);
        close("2*3^2", 18.0);
        close("1,5*2", 3.0);
        close("--2", 2.0);
    }
    #[test]
    fn power_invalid() {
        for e in ["10^400", "0^-1", "(-8)^(1/3)", "1/0", "0/0", "(1+2", "1+2)", "5+", "2^"] {
            assert_eq!(eval(e), None, "{e}");
        }
    }
    #[test]
    fn deep_input_no_overflow() {
        assert_eq!(eval(&"(".repeat(100000)), None);
        assert_eq!(eval(&"-".repeat(100000)), None);
        assert_eq!(eval(&"2^".repeat(300)), None);
        let e = format!("{}1{}", "(".repeat(50), ")".repeat(50));
        close(&e, 1.0);
    }
}

#[cfg(test)]
mod clip_private_tests {
    use super::clip_is_private;

    #[test]
    fn exclude_flag_is_private() {
        assert!(clip_is_private(true, None, None));
        assert!(clip_is_private(true, Some(1), Some(1)));
    }

    #[test]
    fn zero_history_or_cloud_is_private() {
        assert!(clip_is_private(false, Some(0), None));
        assert!(clip_is_private(false, None, Some(0)));
        assert!(clip_is_private(false, Some(0), Some(1)));
        assert!(clip_is_private(false, Some(1), Some(0)));
        assert!(clip_is_private(true, Some(0), Some(0)));
    }

    #[test]
    fn allowed_or_absent_is_not_private() {
        assert!(!clip_is_private(false, None, None));
        assert!(!clip_is_private(false, Some(1), None));
        assert!(!clip_is_private(false, None, Some(1)));
        assert!(!clip_is_private(false, Some(1), Some(1)));
        assert!(!clip_is_private(false, Some(7), Some(u32::MAX)));
    }
}

/// Отметка двух часов: `tick_ms` идёт и во сне, `unbiased_100ns` во сне стоит.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SleepClock {
    pub tick_ms: u64,
    pub unbiased_100ns: u64,
}

/// Сколько компьютер проспал между отметками: прирост «идущих во сне» часов минус прирост «стоящих».
pub fn slept_between(a: SleepClock, b: SleepClock) -> std::time::Duration {
    let wall = std::time::Duration::from_millis(b.tick_ms.saturating_sub(a.tick_ms));
    let awake = std::time::Duration::from_nanos(b.unbiased_100ns.saturating_sub(a.unbiased_100ns).saturating_mul(100));
    wall.saturating_sub(awake)
}

#[link(name = "kernel32")]
extern "system" {
    fn QueryUnbiasedInterruptTime(time: *mut u64) -> i32;
}

/// Текущая отметка часов сна (None, если система не ответила).
pub fn sleep_clock() -> Option<SleepClock> {
    let mut unbiased = 0u64;
    // SAFETY: передаём указатель на свою переменную, функция только пишет в неё.
    let ok = unsafe { QueryUnbiasedInterruptTime(&mut unbiased) } != 0;
    let tick_ms = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
    ok.then_some(SleepClock { tick_ms, unbiased_100ns: unbiased })
}

/// Подклассирует окно (один раз): при завершении сеанса Windows записывает снимок
/// несохранённых заметок и настроек прямо из оконной процедуры, без участия UI.
/// Вызывать из потока окна.
pub fn watch_end_session(hwnd: isize) {
    use windows::Win32::UI::Shell::SetWindowSubclass;
    static DONE: AtomicBool = AtomicBool::new(false);
    if hwnd == 0 || DONE.load(Ordering::Relaxed) {
        return;
    }
    let ok = unsafe { SetWindowSubclass(HWND(hwnd as *mut core::ffi::c_void), Some(end_session_proc), 0x05E5, 0).as_bool() };
    if ok {
        DONE.store(true, Ordering::Relaxed);
    }
}

unsafe extern "system" fn end_session_proc(
    hwnd: HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
    _id: usize,
    _data: usize,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::Shell::DefSubclassProc;
    // WM_QUERYENDSESSION не блокируем — уходит дальше как есть
    if msg == WM_ENDSESSION && wparam.0 != 0 {
        let _ = std::panic::catch_unwind(crate::store::write_snapshot);
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

// ---------------------------------------------------------------- место окна острова

/// Монитор, на котором точка (пиксели экрана): [лево, верх, право, низ].
pub fn monitor_at(x: i32, y: i32) -> [i32; 4] {
    unsafe {
        let mon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let r = mi.rcMonitor;
            [r.left, r.top, r.right, r.bottom]
        } else {
            [0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)]
        }
    }
}

/// Позиция и размер окна одним вызовом (пиксели экрана), без смены порядка окон и фокуса.
pub fn place_window(hwnd: isize, r: [i32; 4]) {
    unsafe {
        let _ = SetWindowPos(
            HWND(hwnd as *mut core::ffi::c_void),
            HWND::default(),
            r[0],
            r[1],
            r[2],
            r[3],
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        );
    }
}

/// Зажата ли левая кнопка мыши (с учётом перестановки кнопок).
pub fn left_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
    let vk = if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 { VK_RBUTTON } else { VK_LBUTTON };
    unsafe { GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0 }
}
