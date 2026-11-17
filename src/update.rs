//! Самообновление: остров сам находит новую версию в релизах на GitHub, скачивает её и подменяет себя.
//!
//! Файл берётся только из релизов своего репозитория по защищённому соединению. Скачанное сверяется
//! по размеру с тем, что заявил GitHub, и должно быть программой Windows. Подмена идёт так: работающий
//! файл переименовывается (Windows это разрешает), на его место кладётся новый и запускается; если
//! что-то не вышло — старый файл возвращается на место.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const API: &str = "https://api.github.com/repos/qutlawsoasis-debug/Ostrovok/releases?per_page=10";
const ASSET_PREFIX: &str = "https://github.com/qutlawsoasis-debug/Ostrovok/releases/download/";
const ASSET: &str = "Ostrovok.exe";
const AGENT: &str = "ostrovok-updater";

/// Метка списка релизов с прошлой проверки. С ней GitHub отвечает «ничего не изменилось» почти даром
/// и не считает такой запрос в свой часовой предел, поэтому спрашивать можно раз в минуту.
static ETAG: Mutex<Option<String>> = Mutex::new(None);

#[derive(Clone, Default)]
pub enum State {
    #[default]
    Idle,
    Checking,
    /// новая версия скачана и ждёт установки: номер и путь к файлу
    Ready(String, PathBuf),
    /// новее ничего нет
    Fresh,
    Failed(String),
}

/// Версия работающей программы. OSTROVOK_FAKE_VERSION подменяет её для проверки обновления.
pub fn current() -> String {
    std::env::var("OSTROVOK_FAKE_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

fn parse(v: &str) -> Option<[u32; 3]> {
    let mut it = v.trim().trim_start_matches('v').split('.').map(|p| p.parse::<u32>().ok());
    Some([it.next()??, it.next()??, it.next()??])
}

pub fn newer(tag: &str, cur: &str) -> bool {
    matches!((parse(tag), parse(cur)), (Some(a), Some(b)) if a > b)
}

fn dir() -> PathBuf {
    crate::store::dir().join("update")
}

fn old_exe() -> Option<PathBuf> {
    std::env::current_exe().ok().map(|e| e.with_extension("old.exe"))
}

/// Ищет версию новее текущей и скачивает её. None — обновляться не на что.
fn check() -> Result<Option<(String, PathBuf)>, String> {
    let net = |e: ureq::Error| match e {
        ureq::Error::Status(c, _) => format!("GitHub ответил {c}"),
        _ => "нет связи с GitHub".to_string(),
    };
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout_read(Duration::from_secs(60)).build();
    let mut req = agent.get(API).set("user-agent", AGENT).set("accept", "application/vnd.github+json");
    if let Some(tag) = ETAG.lock().unwrap().as_deref() {
        req = req.set("if-none-match", tag);
    }
    let resp = req.call().map_err(net)?;
    if resp.status() == 304 {
        return Ok(None);
    }
    let etag = resp.header("etag").map(str::to_string);
    let list: serde_json::Value = resp.into_json().map_err(|_| "непонятный ответ GitHub".to_string())?;
    let cur = current();
    let mut best: Option<([u32; 3], String, String, u64, String)> = None;
    for r in list.as_array().ok_or("непонятный ответ GitHub")? {
        if r["draft"].as_bool() == Some(true) {
            continue;
        }
        let tag = r["tag_name"].as_str().unwrap_or("");
        let Some(ver) = parse(tag) else { continue };
        if !newer(tag, &cur) {
            continue;
        }
        // с этой версии уже пришлось откатиться — не ставим её снова, ждём следующую
        if bad_version().is_some_and(|b| b == tag.trim_start_matches('v')) {
            continue;
        }
        let Some(asset) = r["assets"].as_array().and_then(|a| a.iter().find(|a| a["name"].as_str() == Some(ASSET))) else { continue };
        let url = asset["browser_download_url"].as_str().unwrap_or("");
        if !url.starts_with(ASSET_PREFIX) {
            continue;
        }
        if best.as_ref().map_or(true, |b| ver > b.0) {
            best = Some((ver, tag.trim_start_matches('v').to_string(), url.to_string(), asset["size"].as_u64().unwrap_or(0), r["body"].as_str().unwrap_or("").to_string()));
        }
    }
    let Some((_, version, url, size, notes)) = best else {
        // метку запоминаем, только когда обновляться не на что: иначе неудачная загрузка не повторилась бы
        *ETAG.lock().unwrap() = etag;
        return Ok(None);
    };
    if !(1_000_000..=80_000_000).contains(&size) {
        return Err("у файла обновления странный размер".into());
    }
    let io = |e: std::io::Error| format!("не удалось сохранить обновление: {e}");
    std::fs::create_dir_all(dir()).map_err(io)?;
    let ready = dir().join(format!("Ostrovok-{version}.exe"));
    if std::fs::metadata(&ready).map(|m| m.len()).ok() != Some(size) {
        let part = dir().join("Ostrovok.part");
        let resp = agent.get(&url).set("user-agent", AGENT).call().map_err(net)?;
        let mut file = std::fs::File::create(&part).map_err(io)?;
        let got = std::io::copy(&mut resp.into_reader().take(size + 1), &mut file).map_err(io)?;
        drop(file);
        let mut head = [0u8; 2];
        let is_exe = std::fs::File::open(&part).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head == b"MZ";
        if got != size || !is_exe {
            let _ = std::fs::remove_file(&part);
            return Err("обновление скачалось не полностью".into());
        }
        std::fs::rename(&part, &ready).map_err(io)?;
    }
    // описание релиза кладём рядом со скачанным файлом; в «что нового» оно попадёт только при установке
    let _ = std::fs::write(ready.with_extension("txt"), notes);
    Ok(Some((version, ready)))
}

/// Главное из описания релиза, которым только что обновились: первый заголовок, кроме «Установки».
/// Читается один раз — файл после этого удаляется.
pub fn whats_new() -> Option<String> {
    let path = crate::store::dir().join("whatsnew.txt");
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("## "))
        .map(|h| h.trim().to_string())
        .find(|h| !h.is_empty() && !h.to_lowercase().starts_with("установка"))
}

/// Проверяет обновление в стороне от интерфейса; wake зовётся, когда результат готов.
pub fn spawn(state: Arc<Mutex<State>>, wake: impl Fn() + Send + 'static) {
    {
        let mut s = state.lock().unwrap();
        if matches!(*s, State::Checking | State::Ready(..)) {
            return;
        }
        *s = State::Checking;
    }
    std::thread::spawn(move || {
        let next = match check() {
            Ok(Some((v, p))) => State::Ready(v, p),
            Ok(None) => State::Fresh,
            Err(e) => State::Failed(e),
        };
        *state.lock().unwrap() = next;
        wake();
    });
}

/// Ставит скачанную версию на место работающей и запускает её. После успеха вызывающий должен выйти.
pub fn apply(new: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let old = exe.with_extension("old.exe");
    // сначала целиком копируем рядом: если место кончится, рабочая программа останется нетронутой
    let staged = exe.with_extension("new.exe");
    std::fs::copy(new, &staged).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        format!("не удалось подготовить новую версию: {e}")
    })?;
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        format!("не удалось заменить файл программы: {e}")
    })?;
    if let Err(e) = std::fs::rename(&staged, &exe) {
        let back = std::fs::rename(&old, &exe);
        return Err(match back {
            Ok(_) => format!("не удалось поставить новую версию: {e}"),
            Err(_) => format!("не удалось поставить новую версию, прежняя лежит в {}", old.display()),
        });
    }
    // «что нового» — только теперь, когда версия действительно ставится
    if let Ok(notes) = std::fs::read_to_string(new.with_extension("txt")) {
        let _ = std::fs::write(crate::store::dir().join("whatsnew.txt"), notes);
    }
    // новая версия «на испытании»: отметку ставим ДО запуска, чтобы откат сработал, даже если она упадёт мгновенно
    let version = new.file_stem().and_then(|s| s.to_str()).and_then(|s| s.strip_prefix("Ostrovok-")).unwrap_or("").to_string();
    let _ = std::fs::write(trial_mark(), format!("{}\n{}", version, exe.display()));
    match std::process::Command::new(&exe).arg("--updated").env_remove("OSTROVOK_FAKE_VERSION").spawn() {
        Ok(_) => Ok(()),
        Err(e) => {
            // новая версия не запустилась (например, её остановил антивирус) — возвращаем прежнюю
            let _ = std::fs::remove_file(trial_mark());
            let _ = std::fs::remove_file(crate::store::dir().join("whatsnew.txt"));
            let broken = exe.with_extension("broken.exe");
            let _ = std::fs::remove_file(&broken);
            if std::fs::rename(&exe, &broken).is_ok() && std::fs::rename(&old, &exe).is_err() {
                let _ = std::fs::copy(&old, &exe);
            }
            Err(format!("новая версия не запустилась: {e}"))
        }
    }
}

fn trial_mark() -> PathBuf {
    crate::store::dir().join("update-trial")
}

fn bad_mark() -> PathBuf {
    crate::store::dir().join("update-bad")
}

/// Версия, с которой пришлось откатиться: её обновление пропускает.
fn bad_version() -> Option<String> {
    std::fs::read_to_string(bad_mark()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Испытание пройдено (или остров закрыт штатно) — отметку снимаем, откат больше не нужен.
pub fn trial_passed() {
    let _ = std::fs::remove_file(trial_mark());
}

/// Решает, надо ли откатываться: отметка испытания стоит именно на этот файл программы и на эту версию.
pub fn should_roll_back(mark: &str, exe: &Path, version: &str) -> bool {
    // файл мог сохранить редактор с невидимой меткой BOM в начале
    let mut lines = mark.trim_start_matches('\u{FEFF}').lines();
    let (Some(v), Some(p)) = (lines.next(), lines.next()) else { return false };
    v.trim() == version && Path::new(p.trim()) == exe
}

/// Новая версия упала, не дожив до конца испытания, а прежняя лежит рядом — возвращаем прежнюю и запускаем.
/// Вызывать только когда этот процесс захватил «единственный экземпляр»: значит, новая версия точно не работает.
/// true — откат сделан, этой копии надо выйти.
pub fn rollback_if_failed() -> bool {
    let (Ok(exe), Some(old)) = (std::env::current_exe(), old_exe()) else { return false };
    let Ok(mark) = std::fs::read_to_string(trial_mark()) else { return false };
    if !old.exists() || !should_roll_back(&mark, &exe, &current()) {
        return false;
    }
    let _ = std::fs::remove_file(trial_mark());
    // запоминаем неудачную версию, чтобы прежняя не поставила её снова по кругу, и убираем её «что нового»
    let _ = std::fs::write(bad_mark(), current());
    let _ = std::fs::remove_file(crate::store::dir().join("whatsnew.txt"));
    let broken = exe.with_extension("broken.exe");
    let _ = std::fs::remove_file(&broken);
    if std::fs::rename(&exe, &broken).is_err() {
        return false;
    }
    if std::fs::rename(&old, &exe).is_err() {
        let _ = std::fs::rename(&broken, &exe);
        return false;
    }
    // --updated: прежняя версия подождёт, пока эта копия отпустит «единственный экземпляр»
    std::process::Command::new(&exe).arg("--updated").spawn().is_ok()
}

/// Убирает следы прошлого обновления. Старый файл программы удаляется только после минуты нормальной
/// работы новой версии — до этого он нужен для отката.
pub fn cleanup() {
    std::thread::spawn(|| {
        // падения при запуске случаются в первые секунды: после 10 секунд работы испытание пройдено,
        // и закрытие острова вручную уже не вызовет ложный откат
        std::thread::sleep(Duration::from_secs(10));
        trial_passed();
        std::thread::sleep(Duration::from_secs(50));
        if let Some(old) = old_exe() {
            let _ = std::fs::remove_file(old);
        }
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::fs::remove_file(exe.with_extension("broken.exe"));
        }
        let _ = std::fs::remove_dir_all(dir());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(newer("v0.1.10", "0.1.9"));
        assert!(newer("0.2.0", "0.1.99"));
        assert!(!newer("v0.1.7", "0.1.7"));
        assert!(!newer("v0.1.6", "0.1.7"));
        assert!(!newer("мусор", "0.1.7"));
    }

    #[test]
    fn rollback_only_for_same_exe_and_version() {
        let exe = Path::new(r"C:\Users\a\Ostrovok\Ostrovok.exe");
        let mark = "0.1.46\nC:\\Users\\a\\Ostrovok\\Ostrovok.exe";
        assert!(should_roll_back(mark, exe, "0.1.46"));
        assert!(!should_roll_back(mark, exe, "0.1.45")); // уже откатились или другая версия
        assert!(!should_roll_back(mark, Path::new(r"D:\portable\Ostrovok.exe"), "0.1.46")); // другая копия
        assert!(!should_roll_back("0.1.46", exe, "0.1.46")); // старый формат без пути
        assert!(!should_roll_back("", exe, "0.1.46"));
        assert!(should_roll_back("\u{FEFF}0.1.46\r\nC:\\Users\\a\\Ostrovok\\Ostrovok.exe", exe, "0.1.46"));
    }
}
