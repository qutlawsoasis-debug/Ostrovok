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
    /// быстрые действия на сводке, скрытые правым щелчком (в порядке скрытия)
    #[serde(default)]
    pub hidden_actions: Vec<String>,
    /// показывать на острове уведомления мессенджеров
    #[serde(default)]
    pub mirror: bool,
    /// остров виден на демонстрации и записи экрана (тогда стекло непрозрачное)
    #[serde(default)]
    pub capture_visible: bool,
    /// куда пользователь перетащил остров: по монитору, последний — первым
    #[serde(default)]
    pub spots: Vec<Spot>,
    /// выключить жидкость в пилюле (остров-колбу)
    #[serde(default)]
    pub no_flask: bool,
    /// не показывать в пилюле громкость и смену раскладки
    #[serde(default)]
    pub no_hud: bool,
    /// показывать остров и поверх полноэкранных игр и видео
    #[serde(default)]
    pub over_games: bool,
    /// «Не беспокоить» из «Пульта»: уведомления копятся тихо, переживает перезапуск
    #[serde(default)]
    pub dnd: bool,
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
    /// «Недавнее» строки поиска: только тексты запросов, не больше пяти
    #[serde(default)]
    pub recent: Vec<String>,
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

/// Место острова на одном мониторе: ключ монитора и начало поля острова в пикселях экрана.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Spot {
    pub mon: String,
    pub x: f32,
    pub y: f32,
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
    /// срок «ГГГГ-ММ-ДД», если указан
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Reminder {
    pub text: String,
    /// время срабатывания, секунды с 1970 года
    pub at: i64,
    /// повтор: 0 — нет, 1 — каждый день, 2 — по будням, 3 — каждую неделю
    #[serde(default, skip_serializing_if = "is_zero")]
    pub repeat: u8,
}

fn is_zero(v: &u8) -> bool {
    *v == 0
}

/// Подмена каталога данных (для тестов).
#[cfg(test)]
pub static OVERRIDE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Предупреждение о загрузке настроек — показывается один раз при старте.
pub static LOAD_WARN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Добавляет предупреждение к уже накопленным, не затирая их.
fn add_warn(msg: &str) {
    let mut w = LOAD_WARN.lock().unwrap_or_else(|e| e.into_inner());
    *w = Some(match w.take() {
        Some(old) if !old.is_empty() => format!("{old}\n{msg}"),
        _ => msg.to_string(),
    });
}

pub fn dir() -> PathBuf {
    #[cfg(test)]
    if let Some(p) = OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return p;
    }
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("Ostrovok")
}

/// Надёжная запись: сначала во временный файл, сброс на диск, затем подмена.
/// Если питание пропадёт посреди записи, останется прежний целый файл, а не обрезок.
pub fn write_atomic(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    // своё имя временного файла на каждую запись: два одновременных сохранения
    // не должны писать в один и тот же .tmp и подменять файл обрезком
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("{}.{n}.tmp", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        if let Err(e) = f.write_all(data).and_then(|_| f.sync_all()) {
            drop(f);
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
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
/// Заметки не прочитались — в этом запуске их не перезаписываем.
static NOTES_NO_SAVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// settings.json.bak в этом запуске уже обновлялся (или решено не обновлять).
static BAK_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// errors.log больше этого — уходит в errors.log.old
const LOG_MAX: u64 = 1024 * 1024;
/// сколько копий испорченного файла хранить
const KEEP_COPIES: usize = 5;

/// Запись в errors.log, чтобы сбои записи не терялись молча.
pub fn log_err(text: &str) {
    use std::io::Write;
    if !test_free() {
        return;
    }
    let d = dir();
    let _ = std::fs::create_dir_all(&d);
    let log = d.join("errors.log");
    if std::fs::metadata(&log).map(|m| m.len() > LOG_MAX).unwrap_or(false) {
        let _ = std::fs::rename(&log, d.join("errors.log.old"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
        let _ = writeln!(f, "[{}] данные: {text}", unix_now());
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Копия испорченных байт файла `name` рядом (`name.bak-<время>`), без перезаписи прежних копий.
/// Хранятся последние KEEP_COPIES копий.
fn keep_corrupt_copy(name: &str, bytes: &[u8]) -> bool {
    use std::io::Write;
    if !test_free() {
        return false;
    }
    let d = dir();
    let _ = std::fs::create_dir_all(&d);
    let t = unix_now();
    // номер после уже занятых в эту секунду, чтобы новая копия всегда была «новее» оставшихся
    let first = std::fs::read_dir(&d)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| copy_index(&e.file_name().to_string_lossy(), name))
                .filter(|k| k.0 == t)
                .map(|k| k.1 + 1)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    for i in first..first + 100 {
        let file = if i == 0 { format!("{name}.bak-{t}") } else { format!("{name}.bak-{t}-{i}") };
        match std::fs::OpenOptions::new().write(true).create_new(true).open(d.join(file)) {
            Ok(mut f) => {
                let ok = f.write_all(bytes).and_then(|_| f.sync_all()).is_ok();
                drop(f);
                prune_copies(&d, name);
                return ok;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return false,
        }
    }
    false
}

/// Удаляет старые копии `name.bak-*`, оставляя KEEP_COPIES самых новых.
fn prune_copies(d: &std::path::Path, name: &str) {
    let Ok(rd) = std::fs::read_dir(d) else { return };
    let mut list: Vec<((u64, u64), PathBuf)> = rd
        .flatten()
        .filter_map(|e| Some((copy_index(&e.file_name().to_string_lossy(), name)?, e.path())))
        .collect();
    if list.len() <= KEEP_COPIES {
        return;
    }
    list.sort_by_key(|x| x.0);
    let extra = list.len() - KEEP_COPIES;
    for (_, p) in list.into_iter().take(extra) {
        let _ = std::fs::remove_file(p);
    }
}

/// (время, номер) из имени копии «<name>.bak-<время>[-<номер>]».
fn copy_index(file: &str, name: &str) -> Option<(u64, u64)> {
    let tail = file.strip_prefix(name)?.strip_prefix(".bak-")?;
    let mut it = tail.splitn(2, '-');
    let t = it.next()?.parse::<u64>().ok()?;
    let i = match it.next() {
        Some(x) => x.parse::<u64>().ok()?,
        None => 0,
    };
    Some((t, i))
}

impl Settings {
    pub fn load() -> Self {
        let path = dir().join("settings.json");
        let mut bytes = None;
        for _ in 0..5 {
            match std::fs::read(&path) {
                Ok(b) => {
                    bytes = Some(b);
                    break;
                }
                // файла нет — первый запуск, это нормально
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
                // файл занят (антивирус, индексатор) — подождать и повторить
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(150)),
            }
        }
        let Some(bytes) = bytes else {
            NO_SAVE.store(true, std::sync::atomic::Ordering::Relaxed);
            log_err("settings.json не читается — сохранение в этом запуске отключено");
            add_warn("Настройки не удалось прочитать — изменения в этом запуске не запишутся");
            return Self::default();
        };
        if let Ok(s) = serde_json::from_slice(&bytes) {
            return s;
        }
        // файл испорчен — копию исходных байт откладываем в сторону, а не затираем
        let kept = keep_corrupt_copy("settings.json", &bytes);
        if kept {
            log_err("settings.json испорчен — копия сохранена в settings.json.bak-*");
        } else {
            NO_SAVE.store(true, std::sync::atomic::Ordering::Relaxed);
            log_err("settings.json испорчен, копию сделать не удалось — сохранение отключено");
        }
        let restored = std::fs::read(dir().join("settings.json.bak"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Settings>(&b).ok());
        let msg = match (&restored, kept) {
            (Some(_), true) => "Настройки были повреждены — копия сохранена, восстановлены из резервной",
            (None, true) => "Настройки были повреждены — копия сохранена, заданы заново",
            (_, false) => "Настройки были повреждены — копию сохранить не удалось, изменения не запишутся",
        };
        add_warn(msg);
        restored.unwrap_or_default()
    }

    pub fn save(&self) {
        use std::sync::atomic::Ordering;
        // проверочный запуск не должен менять настоящие настройки
        if !test_free() || NO_SAVE.load(Ordering::Relaxed) {
            return;
        }
        let _ = std::fs::create_dir_all(dir());
        let path = dir().join("settings.json");
        // резервная копия — один раз за запуск, при первом сохранении, только если нынешний файл исправен
        if !BAK_DONE.swap(true, Ordering::Relaxed) {
            if let Ok(cur) = std::fs::read(&path) {
                if serde_json::from_slice::<Settings>(&cur).is_ok() {
                    if let Err(e) = write_atomic(&dir().join("settings.json.bak"), &cur) {
                        log_err(&format!("не удалось обновить settings.json.bak: {e}"));
                    }
                }
            }
        }
        match serde_json::to_string_pretty(self) {
            Ok(s) => {
                // снимок для записи при завершении сеанса Windows держим, только пока на диск не легло
                let snap = match write_atomic(&path, s.as_bytes()) {
                    Ok(()) => None,
                    Err(e) => {
                        log_err(&format!("не удалось записать settings.json: {e}"));
                        Some(s.into_bytes())
                    }
                };
                *SNAP_SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = snap;
            }
            Err(e) => log_err(&format!("не удалось собрать settings.json: {e}")),
        }
    }
}

fn test_free() -> bool {
    std::env::var_os("OSTROVOK_TEST").is_none()
}

#[cfg(test)]
mod tests {
    use super::{due_flush, Flush, FLUSH_AFTER};
    use std::time::{Duration, Instant};

    #[test]
    fn due_flush_thresholds() {
        let t0 = Instant::now();
        assert_eq!(due_flush(None, t0), Flush::Idle);
        assert_eq!(due_flush(Some(t0), t0), Flush::WaitFor(FLUSH_AFTER));
        assert_eq!(due_flush(Some(t0), t0 + Duration::from_millis(1490)), Flush::WaitFor(Duration::from_millis(10)));
        assert_eq!(due_flush(Some(t0), t0 + Duration::from_millis(1500)), Flush::Save);
        assert_eq!(due_flush(Some(t0), t0 + Duration::from_secs(10)), Flush::Save);
    }

    #[test]
    fn continuous_typing_saves_regularly() {
        // правка каждые 100 мс в течение 5 с; флаг ставится только если пуст
        let t0 = Instant::now();
        let mut dirty: Option<Instant> = None;
        let mut saves = vec![];
        for i in 0..=50u64 {
            let now = t0 + Duration::from_millis(i * 100);
            dirty.get_or_insert(now);
            if due_flush(dirty, now) == Flush::Save {
                assert!(now.duration_since(dirty.unwrap()) <= FLUSH_AFTER, "правка ждала дольше 1.5 с");
                saves.push(now);
                dirty = None;
            }
        }
        assert!(saves.len() >= 3, "сохранений: {}", saves.len());
        let mut prev = t0;
        for s in &saves {
            // между сохранениями — 1.5 с плюс не более одного шага набора
            assert!(s.duration_since(prev) <= FLUSH_AFTER + Duration::from_millis(100), "интервал слишком велик");
            prev = *s;
        }
    }

    #[test]
    fn atomic_write_replaces_file() {
        let dir = std::env::temp_dir().join(format!("ostrovok-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.json");
        super::write_atomic(&path, b"first").unwrap();
        super::write_atomic(&path, "второй".as_bytes()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "второй");
        let hs: Vec<_> = (0..8).map(|i| { let p = path.clone(); std::thread::spawn(move || super::write_atomic(&p, format!("поток {i} ").repeat(5000).as_bytes()).unwrap()) }).collect();
        hs.into_iter().for_each(|h| h.join().unwrap());
        let got = std::fs::read_to_string(&path).unwrap();
        assert!((0..8).any(|i| got == format!("поток {i} ").repeat(5000)), "файл склеен из разных записей");
        let tmp = std::fs::read_dir(&dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".tmp")).count();
        assert_eq!(tmp, 0, "остались временные файлы");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod data_tests {
    use super::*;

    #[test]
    fn dnd_survives_restart() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert!(!s.dnd);
        let s: Settings = serde_json::from_str(r#"{"dnd":true}"#).unwrap();
        assert!(s.dnd);
        let again: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert!(again.dnd);
    }
    use std::sync::atomic::Ordering;

    /// общий замок: каталог данных и флаги — глобальные
    pub static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct Tmp(PathBuf, #[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
    impl Drop for Tmp {
        fn drop(&mut self) {
            *OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) = None;
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn tmp(name: &str) -> Tmp {
        let g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = std::env::temp_dir().join(format!("ostrovok-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        *OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) = Some(d.clone());
        NO_SAVE.store(false, Ordering::Relaxed);
        NOTES_NO_SAVE.store(false, Ordering::Relaxed);
        BAK_DONE.store(false, Ordering::Relaxed);
        *LOAD_WARN.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Tmp(d, g)
    }
    #[test]
    fn snapshot_written_and_loads_back() {
        let t = tmp("snap");
        clear_snapshot();
        assert_eq!(write_snapshot(), 0, "пустой снимок ничего не пишет");
        assert!(std::fs::read_dir(&t.0).unwrap().next().is_none());
        let mut s = Settings::default();
        s.city = "Снимокград".into();
        s.save();
        // успешно записанные настройки снимок не держит — при выходе из Windows их не переписываем
        set_snapshot_notes(&["первая".to_string(), "вторая".to_string()]);
        assert_eq!(write_snapshot(), 1);
        assert_eq!(load_notes(), vec!["первая".to_string(), "вторая".to_string()]);
        assert_eq!(Settings::load().city, "Снимокград");
        no_tmp(&t.0);
        clear_snapshot();
    }

    #[test]
    fn snapshot_respects_notes_no_save() {
        let t = tmp("snap-nosave");
        clear_snapshot();
        set_snapshot_notes(&["x".to_string()]);
        NOTES_NO_SAVE.store(true, Ordering::Relaxed);
        assert_eq!(write_snapshot(), 0);
        assert!(!t.0.join("notes.txt").exists());
        clear_snapshot();
    }

    fn baks(d: &std::path::Path) -> Vec<PathBuf> {
        std::fs::read_dir(d).unwrap().flatten().map(|e| e.path()).filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("settings.json.bak-")).collect()
    }
    fn no_tmp(d: &std::path::Path) {
        assert!(!std::fs::read_dir(d).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".tmp")));
    }

    #[test]
    fn settings_roundtrip() {
        let t = tmp("rt");
        let mut s = Settings::default();
        s.todos.push(Todo { text: "дело".into(), done: true, due: Some("2026-11-18".into()) });
        s.shelf.push(r"C:\файл.txt".into());
        s.reminders.push(Reminder { text: "звонок".into(), at: 42, repeat: 1 });
        s.save();
        BAK_DONE.store(false, Ordering::Relaxed); // «следующий запуск»
        s.save();
        let l = Settings::load();
        assert_eq!(l.todos[0].text, "дело");
        assert!(l.todos[0].done);
        assert_eq!(l.todos[0].due.as_deref(), Some("2026-11-18"));
        assert_eq!(l.shelf, s.shelf);
        assert_eq!((l.reminders[0].at, l.reminders[0].repeat), (42, 1));
        assert!(t.0.join("settings.json.bak").exists());
        no_tmp(&t.0);
    }

    #[test]
    fn corrupt_settings_keeps_backup() {
        let t = tmp("bad");
        let junk: &[u8] = b"{ \"city\": \xff\xfe broken";
        std::fs::write(t.0.join("settings.json"), junk).unwrap();
        let s = Settings::load();
        let b = baks(&t.0);
        assert_eq!(b.len(), 1);
        assert_eq!(std::fs::read(&b[0]).unwrap(), junk);
        let w = LOAD_WARN.lock().unwrap().clone().unwrap();
        assert!(w.starts_with("Настройки были повреждены — копия сохранена"), "{w}");
        s.save();
        s.save();
        assert_eq!(std::fs::read(&b[0]).unwrap(), junk);
        assert_eq!(baks(&t.0).len(), 1);
        // испорченный файл не попал в .bak
        assert!(!t.0.join("settings.json.bak").exists());
        no_tmp(&t.0);
    }

    #[test]
    fn corrupt_restores_from_bak() {
        let t = tmp("restore");
        let mut s = Settings::default();
        s.todos.push(Todo { text: "важное".into(), done: false, due: None });
        s.save();
        BAK_DONE.store(false, Ordering::Relaxed); // «следующий запуск»
        s.save(); // теперь .bak содержит дела
        std::fs::write(t.0.join("settings.json"), b"\0\0\0").unwrap();
        let l = Settings::load();
        assert_eq!(l.todos[0].text, "важное");
        assert_eq!(baks(&t.0).len(), 1);
        assert!(LOAD_WARN.lock().unwrap().as_deref().unwrap().contains("восстановлены"));
    }

    #[test]
    fn notes_roundtrip() {
        let t = tmp("notes");
        save_notes(&["первая".into(), "  ".into(), "вторая\nстрока".into()]);
        assert_eq!(load_notes(), vec!["первая".to_string(), "вторая\nстрока".to_string()]);
        no_tmp(&t.0);
    }

    #[test]
    fn notes_bad_utf8_not_erased() {
        let t = tmp("notes8");
        std::fs::write(t.0.join("notes.txt"), b"hello \xff world").unwrap();
        let n = load_notes();
        assert_eq!(n.len(), 1);
        assert!(n[0].starts_with("hello") && n[0].ends_with("world"));
    }

    #[test]
    fn notes_bad_utf8_copy_survives_save() {
        let t = tmp("notes8c");
        let raw: &[u8] = b"hello \xff world";
        std::fs::write(t.0.join("notes.txt"), raw).unwrap();
        let n = load_notes();
        save_notes(&n);
        let copies: Vec<_> = std::fs::read_dir(&t.0).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("notes.txt.bak-")).collect();
        assert_eq!(copies.len(), 1);
        assert_eq!(std::fs::read(copies[0].path()).unwrap(), raw);
        assert!(std::fs::read(t.0.join("notes.txt")).unwrap() != raw);
    }

    #[test]
    fn corrupt_copies_limited_to_five() {
        let t = tmp("prune");
        for i in 0..8u8 {
            assert!(keep_corrupt_copy("settings.json", &[i]));
        }
        let b = baks(&t.0);
        assert_eq!(b.len(), 5);
        let mut got: Vec<u8> = b.iter().map(|p| std::fs::read(p).unwrap()[0]).collect();
        got.sort();
        assert_eq!(got, vec![3, 4, 5, 6, 7]);
    }

    #[test]
    fn big_log_rotated() {
        let t = tmp("log");
        std::fs::write(t.0.join("errors.log"), vec![b'x'; (LOG_MAX + 1) as usize]).unwrap();
        log_err("новое");
        assert_eq!(std::fs::metadata(t.0.join("errors.log.old")).unwrap().len(), LOG_MAX + 1);
        let l = std::fs::read_to_string(t.0.join("errors.log")).unwrap();
        assert!(l.contains("новое") && l.len() < 200);
    }

    #[test]
    fn bak_updated_once_per_run() {
        let t = tmp("once");
        let mut s = Settings::default();
        s.city = "a".into();
        s.save(); // файла не было — .bak нет
        assert!(!t.0.join("settings.json.bak").exists());
        s.city = "b".into();
        s.save(); // флаг уже взведён — .bak не появляется
        assert!(!t.0.join("settings.json.bak").exists());
        BAK_DONE.store(false, Ordering::Relaxed);
        s.city = "c".into();
        s.save();
        s.city = "d".into();
        s.save();
        let bak: Settings = serde_json::from_slice(&std::fs::read(t.0.join("settings.json.bak")).unwrap()).unwrap();
        assert_eq!(bak.city, "b");
    }

    #[test]
    fn warnings_accumulate() {
        let t = tmp("warn");
        add_warn("первое");
        std::fs::create_dir_all(t.0.join("notes.txt")).unwrap();
        let _ = load_notes();
        let w = LOAD_WARN.lock().unwrap().clone().unwrap();
        assert!(w.starts_with("первое\n") && w.contains("Заметки"), "{w}");
    }

    #[test]
    fn unreadable_notes_block_save() {
        let t = tmp("notesdir");
        // каталог вместо файла — чтение падает не с NotFound
        std::fs::create_dir_all(t.0.join("notes.txt")).unwrap();
        let _ = load_notes();
        save_notes(&["x".into()]);
        assert!(t.0.join("notes.txt").is_dir());
        assert!(std::fs::read_to_string(t.0.join("errors.log")).unwrap().contains("notes.txt"));
    }
}

/// Разделитель заметок в файле: служебный знак «конец записи» на отдельной строке.
/// Файл со старой единственной заметкой читается как одна заметка.
const NOTE_SEP: &str = "\n\u{1E}\n";

pub fn load_notes() -> Vec<String> {
    let path = dir().join("notes.txt");
    let mut text = String::new();
    let mut failed = None;
    for _ in 0..5 {
        match std::fs::read(&path) {
            Ok(b) => {
                text = match String::from_utf8(b) {
                    Ok(t) => t,
                    Err(e) => {
                        // байты не UTF-8 — исходник откладываем, чтобы сохранение не стёрло его
                        let b = e.into_bytes();
                        if keep_corrupt_copy("notes.txt", &b) {
                            log_err("notes.txt не в UTF-8 — копия сохранена в notes.txt.bak-*");
                        } else {
                            NOTES_NO_SAVE.store(true, std::sync::atomic::Ordering::Relaxed);
                            log_err("notes.txt не в UTF-8, копию сделать не удалось — сохранение заметок отключено");
                            add_warn("Заметки повреждены — копию сохранить не удалось, изменения не запишутся");
                        }
                        String::from_utf8_lossy(&b).into_owned()
                    }
                };
                failed = None;
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                failed = None;
                break;
            }
            Err(e) => {
                failed = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        }
    }
    if let Some(e) = failed {
        NOTES_NO_SAVE.store(true, std::sync::atomic::Ordering::Relaxed);
        log_err(&format!("notes.txt не читается ({e}) — сохранение заметок отключено"));
        add_warn("Заметки не удалось прочитать — изменения в этом запуске не запишутся");
    }
    let list: Vec<String> = text.split(NOTE_SEP).map(str::to_string).filter(|n| !n.trim().is_empty()).collect();
    if list.is_empty() { vec![String::new()] } else { list }
}

fn notes_text(notes: &[String]) -> String {
    let kept: Vec<&str> = notes.iter().map(String::as_str).filter(|n| !n.trim().is_empty()).collect();
    kept.join(NOTE_SEP)
}

fn write_notes_text(text: &str) -> bool {
    // проверочный запуск не должен менять настоящие заметки; запрет — тоже «успех», чтобы не крутиться
    if !test_free() || NOTES_NO_SAVE.load(std::sync::atomic::Ordering::Relaxed) {
        return true;
    }
    let _ = std::fs::create_dir_all(dir());
    match write_atomic(&dir().join("notes.txt"), text.as_bytes()) {
        Ok(()) => true,
        Err(e) => {
            log_err(&format!("не удалось записать notes.txt: {e}"));
            false
        }
    }
}

/// Сохраняет заметки. false — запись не удалась (стоит повторить позже).
pub fn save_notes(notes: &[String]) -> bool {
    let text = notes_text(notes);
    let ok = write_notes_text(&text);
    if ok {
        *SNAP_NOTES.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    ok
}

/// Через сколько после первой несохранённой правки заметки пишутся на диск.
pub const FLUSH_AFTER: std::time::Duration = std::time::Duration::from_millis(1500);

#[derive(Debug, PartialEq)]
pub enum Flush {
    Save,
    WaitFor(std::time::Duration),
    Idle,
}

/// Пора ли сохранять заметки: `since` — момент первой несохранённой правки.
pub fn due_flush(since: Option<std::time::Instant>, now: std::time::Instant) -> Flush {
    match since {
        None => Flush::Idle,
        Some(t) => {
            let el = now.saturating_duration_since(t);
            if el >= FLUSH_AFTER { Flush::Save } else { Flush::WaitFor(FLUSH_AFTER - el) }
        }
    }
}

/// Снимок для записи при завершении сеанса Windows (без участия UI).
static SNAP_NOTES: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static SNAP_SETTINGS: std::sync::Mutex<Option<Vec<u8>>> = std::sync::Mutex::new(None);

/// Запомнить текущий текст несохранённых заметок.
pub fn set_snapshot_notes(notes: &[String]) {
    *SNAP_NOTES.lock().unwrap_or_else(|e| e.into_inner()) = Some(notes_text(notes));
}

#[cfg(test)]
fn clear_snapshot() {
    *SNAP_NOTES.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *SNAP_SETTINGS.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Пишет снимок на диск (вызывается из WM_ENDSESSION). Возвращает число записанных файлов.
pub fn write_snapshot() -> usize {
    let mut n = 0;
    if !test_free() {
        return 0;
    }
    let notes = SNAP_NOTES.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(text) = notes {
        if !NOTES_NO_SAVE.load(std::sync::atomic::Ordering::Relaxed) && write_notes_text(&text) {
            n += 1;
        }
    }
    let settings = SNAP_SETTINGS.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(bytes) = settings {
        if !NO_SAVE.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = std::fs::create_dir_all(dir());
            if write_atomic(&dir().join("settings.json"), &bytes).is_ok() {
                n += 1;
            }
        }
    }
    n
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

/// Строка для превью в списке: у пункта чек-листа без разметки «- [ ]».
pub fn preview_line(line: &str) -> String {
    parse_check(line).map_or_else(|| line.trim().to_string(), |(_, label)| label.to_string())
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
    fn preview_strips_markup() {
        assert_eq!(preview_line("- [x] хлеб"), "хлеб");
        assert_eq!(preview_line("  * [ ] сыр "), "сыр");
        assert_eq!(preview_line("просто текст"), "просто текст");
        assert_eq!(preview_line("- [ ]"), "");
    }

    #[test]
    fn appends() {
        assert_eq!(append_check(""), "- [ ] ");
        assert_eq!(append_check("T\n\n"), "T\n- [ ] ");
    }
    #[test]
    fn run_value_quotes_spaces_and_cyrillic() {
        let p = std::path::Path::new(r"C:\Program Files\Островок\ostrovok.exe");
        assert_eq!(run_value(p), r#""C:\Program Files\Островок\ostrovok.exe""#);
    }
}

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "Ostrovok";

/// Значение для ключа Run: путь в кавычках.
pub fn run_value(exe: &std::path::Path) -> String {
    format!("\"{}\"", exe.display())
}

/// Есть ли запись автозапуска. Путь не сверяем: установщик по этой отметке переносит
/// автозапуск на установленную копию, даже если запущен другой скачанный файл.
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
        let _ = key.set_value(RUN_NAME, &run_value(&exe));
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
            let _ = key.set_value(RUN_NAME, &run_value(&exe));
        }
    } else {
        let _ = key.delete_value(RUN_NAME);
    }
}
