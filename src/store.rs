//! Настройки и заметки в %APPDATA%\Ostrovok, автозапуск.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Settings {
    #[serde(default)]
    pub city: String,
    #[serde(default)]
    pub city_name: String,
    /// город выбран новой версией (или уже перепроверен по часовому поясу) — больше его не трогаем
    #[serde(default)]
    pub city_checked: bool,
    /// минуты «Фокуса» по дням: («2026-11-17», 75), последние 60 дней
    #[serde(default)]
    pub focus_log: Vec<(String, u32)>,
    /// города для мировых часов под календарём
    #[serde(default)]
    pub world: Vec<String>,
    #[serde(default)]
    pub lat: Option<f64>,
    #[serde(default)]
    pub lon: Option<f64>,
    /// ключ Claude API, введённый пользователем в настройках
    #[serde(default)]
    pub api_key: String,
    /// чат, который открывается в браузере: claude, gemini или chatgpt
    #[serde(default)]
    pub ai_site: String,
    #[serde(default)]
    pub reminders: Vec<Reminder>,
    /// файлы на полке
    #[serde(default)]
    pub shelf: Vec<String>,
    /// номер персонажа в пилюле
    #[serde(default)]
    pub character: u8,
    /// показывать текст песен (название трека уходит на lrclib.net)
    #[serde(default)]
    pub lyrics: bool,
    /// закреплённые приложения на сводке
    #[serde(default)]
    pub apps: Vec<PinnedApp>,
    /// показывать на острове уведомления мессенджеров
    #[serde(default)]
    pub mirror: bool,
    /// остров виден на демонстрации и записи экрана (тогда стекло непрозрачное)
    #[serde(default)]
    pub capture_visible: bool,
    /// выключить жидкость в пилюле (остров-колбу)
    #[serde(default)]
    pub no_flask: bool,
    /// не показывать в пилюле громкость и смену раскладки
    #[serde(default)]
    pub no_hud: bool,
    /// показывать остров и поверх полноэкранных игр и видео
    #[serde(default)]
    pub over_games: bool,
    /// не рисовать в пилюле дождь и снег по погоде
    #[serde(default)]
    pub no_wx: bool,
    /// список дел
    #[serde(default)]
    pub todos: Vec<Todo>,
    /// прозрачность стекла, 1…100; 0 — не задана (тогда 60)
    #[serde(default)]
    pub clear: u8,
    /// не обновляться с GitHub самостоятельно
    #[serde(default)]
    pub no_update: bool,
    /// версия, которая запускалась в прошлый раз, — чтобы сказать «обновлено»
    #[serde(default)]
    pub last_version: String,
    /// номер темы оформления
    #[serde(default)]
    pub theme: u8,
    /// закреплённые записи буфера обмена — переживают перезапуск
    #[serde(default)]
    pub pins: Vec<String>,
    /// папка с движком и моделями Джарвиса; пусто — рядом с настройками
    #[serde(default)]
    pub jarvis_dir: String,
    /// размер модели Джарвиса: small, medium или large
    #[serde(default)]
    pub jarvis_model: String,
    /// считать только на процессоре, без видеокарты
    #[serde(default)]
    pub jarvis_cpu: bool,
    /// Джарвис отвечает только текстом, без голоса
    #[serde(default)]
    pub jarvis_mute: bool,
    /// постоянно слушать микрофон и откликаться на слово «Джарвис»
    #[serde(default)]
    pub jarvis_listen: bool,
    /// облачный мозг: адрес сервера с протоколом чата OpenAI; пусто — думать только на компьютере
    #[serde(default)]
    pub jarvis_url: String,
    /// ключ для этого сервера (своему посреднику не нужен)
    #[serde(default)]
    pub jarvis_key: String,
    /// имя модели на сервере (посредник подставляет сам)
    #[serde(default)]
    pub jarvis_remote_model: String,
    /// свой ключ Fish Audio и номер голоса: если заданы, ответы озвучивает их сервер
    #[serde(default)]
    pub fish_key: String,
    #[serde(default)]
    pub fish_voice: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PinnedApp {
    pub name: String,
    pub path: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Todo {
    pub text: String,
    #[serde(default)]
    pub done: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Reminder {
    pub text: String,
    /// время срабатывания, секунды с 1970 года
    pub at: i64,
}

pub fn dir() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("Ostrovok")
}

/// Надёжная запись: сначала во временный файл, сброс на диск, затем подмена.
/// Если питание пропадёт посреди записи, останется прежний целый файл, а не обрезок.
pub fn write_atomic(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    // антивирус или OneDrive могут на мгновение держать файл — пробуем несколько раз
    let mut last = Ok(());
    for _ in 0..10 {
        last = std::fs::rename(&tmp, path);
        if last.is_ok() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
    let _ = std::fs::remove_file(&tmp);
    last
}

/// Настройки не прочитались по непонятной причине — в этом запуске их не перезаписываем,
/// чтобы не затереть настоящие пустыми.
static NO_SAVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl Settings {
    pub fn load() -> Self {
        let path = dir().join("settings.json");
        let mut text = None;
        for _ in 0..5 {
            match std::fs::read(&path) {
                Ok(bytes) => {
                    text = Some(String::from_utf8_lossy(&bytes).into_owned());
                    break;
                }
                // файла нет — первый запуск, это нормально
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
                // файл занят (антивирус, индексатор) — подождать и повторить
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(150)),
            }
        }
        let Some(text) = text else {
            NO_SAVE.store(true, std::sync::atomic::Ordering::Relaxed);
            return Self::default();
        };
        match serde_json::from_str(&text) {
            Ok(s) => s,
            Err(_) => {
                // файл испорчен — откладываем его в сторону, а не затираем при первом сохранении
                if test_free() {
                    let _ = std::fs::copy(&path, dir().join("settings.bad.json"));
                }
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        // проверочный запуск не должен менять настоящие настройки
        if !test_free() || NO_SAVE.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let _ = std::fs::create_dir_all(dir());
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = write_atomic(&dir().join("settings.json"), s.as_bytes());
        }
    }
}

fn test_free() -> bool {
    std::env::var_os("OSTROVOK_TEST").is_none()
}

#[cfg(test)]
mod tests {
    #[test]
    fn atomic_write_replaces_file() {
        let dir = std::env::temp_dir().join(format!("ostrovok-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.json");
        super::write_atomic(&path, b"first").unwrap();
        super::write_atomic(&path, "второй".as_bytes()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "второй");
        assert!(!path.with_extension("tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Разделитель заметок в файле: служебный знак «конец записи» на отдельной строке.
/// Файл со старой единственной заметкой читается как одна заметка.
const NOTE_SEP: &str = "\n\u{1E}\n";

pub fn load_notes() -> Vec<String> {
    let text = std::fs::read_to_string(dir().join("notes.txt")).unwrap_or_default();
    let list: Vec<String> = text.split(NOTE_SEP).map(str::to_string).filter(|n| !n.trim().is_empty()).collect();
    if list.is_empty() { vec![String::new()] } else { list }
}

pub fn save_notes(notes: &[String]) {
    // проверочный запуск не должен менять настоящие заметки
    if std::env::var_os("OSTROVOK_TEST").is_some() {
        return;
    }
    let _ = std::fs::create_dir_all(dir());
    let kept: Vec<&str> = notes.iter().map(String::as_str).filter(|n| !n.trim().is_empty()).collect();
    let _ = write_atomic(&dir().join("notes.txt"), kept.join(NOTE_SEP).as_bytes());
}

/// Пункт чек-листа в заметке: номер строки, отмечен ли, текст после «[ ]».
#[derive(Debug, PartialEq)]
pub struct CheckItem {
    pub line: usize,
    pub done: bool,
    pub label: String,
}

/// Разбирает строку «- [ ] текст» / «* [x] текст» (с отступом или без).
fn parse_check(line: &str) -> Option<(bool, &str)> {
    let t = line.trim_start();
    let t = t.strip_prefix("- ").or_else(|| t.strip_prefix("* "))?;
    let (done, rest) = if let Some(r) = t.strip_prefix("[ ]") {
        (false, r)
    } else if let Some(r) = t.strip_prefix("[x]").or_else(|| t.strip_prefix("[X]")) {
        (true, r)
    } else {
        return None;
    };
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    Some((done, rest.trim()))
}

pub fn check_items(note: &str) -> Vec<CheckItem> {
    note.lines()
        .enumerate()
        .filter_map(|(i, l)| parse_check(l).map(|(done, label)| CheckItem { line: i, done, label: label.to_string() }))
        .collect()
}

/// (отмечено, всего)
pub fn check_progress(note: &str) -> (usize, usize) {
    let items = check_items(note);
    (items.iter().filter(|c| c.done).count(), items.len())
}

/// Переключает отметку в строке `line`; прочий текст (и перевод строки в конце) не трогает.
pub fn toggle_check(note: &str, line: usize) -> String {
    let mut out: Vec<String> = note.split('\n').map(str::to_string).collect();
    if let Some(l) = out.get_mut(line) {
        if let Some((done, _)) = parse_check(l.trim_end_matches('\r')) {
            // «[» стоит сразу после отступа и «- »: все символы там ASCII
            let at = l.len() - l.trim_start().len() + 2;
            l.replace_range(at..at + 3, if done { "[ ]" } else { "[x]" });
        }
    }
    out.join("\n")
}

/// Добавляет пустой пункт в конец заметки.
pub fn append_check(note: &str) -> String {
    let base = note.trim_end_matches(['\n', '\r', ' ']);
    if base.is_empty() { "- [ ] ".to_string() } else { format!("{base}\n- [ ] ") }
}

#[cfg(test)]
mod check_tests {
    use super::*;

    #[test]
    fn parses_items() {
        let n = "Покупки\n- [ ] хлеб\n- [x] молоко\n  * [X] сыр\n-[ ] нет\n- [y] нет\n- [ ]слитно";
        let it = check_items(n);
        assert_eq!(it.len(), 3);
        assert_eq!(it[0], CheckItem { line: 1, done: false, label: "хлеб".into() });
        assert!(it[1].done && it[2].done);
        assert_eq!(it[2].label, "сыр");
        assert_eq!(check_progress(n), (2, 3));
        assert_eq!(check_progress("просто текст"), (0, 0));
    }

    #[test]
    fn toggles_only_that_line() {
        let n = "T\n- [ ] а [ ] б\n- [X] в\n";
        let a = toggle_check(n, 1);
        assert_eq!(a, "T\n- [x] а [ ] б\n- [X] в\n");
        assert_eq!(toggle_check(&a, 2), "T\n- [x] а [ ] б\n- [ ] в\n");
        assert_eq!(toggle_check(n, 0), n);
        assert_eq!(toggle_check(n, 99), n);
        assert_eq!(toggle_check("- [ ] а\r\nб", 0), "- [x] а\r\nб");
        assert_eq!(toggle_check("  * [X] а [x]", 0), "  * [ ] а [x]");
    }

    #[test]
    fn appends() {
        assert_eq!(append_check(""), "- [ ] ");
        assert_eq!(append_check("T\n\n"), "T\n- [ ] ");
    }
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "Ostrovok";

pub fn autostart_enabled() -> bool {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|k| k.get_value::<String, _>(RUN_NAME))
        .is_ok()
}

/// Переносит автозапуск на другой файл (после установки в свою папку).
pub fn set_autostart_path(exe: &std::path::Path) {
    use winreg::{enums::*, RegKey};
    if let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE) {
        let _ = key.set_value(RUN_NAME, &format!("\"{}\"", exe.display()));
    }
}

/// Меняется только по галочке в настройках.
pub fn set_autostart(on: bool) {
    use winreg::{enums::*, RegKey};
    let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE) else {
        return;
    };
    if on {
        if let Ok(exe) = std::env::current_exe() {
            let _ = key.set_value(RUN_NAME, &format!("\"{}\"", exe.display()));
        }
    } else {
        let _ = key.delete_value(RUN_NAME);
    }
}
