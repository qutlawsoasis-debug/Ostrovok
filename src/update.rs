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
    let list: serde_json::Value =
        agent.get(API).set("user-agent", AGENT).set("accept", "application/vnd.github+json").call().map_err(net)?.into_json().map_err(|_| "непонятный ответ GitHub".to_string())?;
    let cur = current();
    let mut best: Option<([u32; 3], String, String, u64)> = None;
    for r in list.as_array().ok_or("непонятный ответ GitHub")? {
        if r["draft"].as_bool() == Some(true) {
            continue;
        }
        let tag = r["tag_name"].as_str().unwrap_or("");
        let Some(ver) = parse(tag) else { continue };
        if !newer(tag, &cur) {
            continue;
        }
        let Some(asset) = r["assets"].as_array().and_then(|a| a.iter().find(|a| a["name"].as_str() == Some(ASSET))) else { continue };
        let url = asset["browser_download_url"].as_str().unwrap_or("");
        if !url.starts_with(ASSET_PREFIX) {
            continue;
        }
        if best.as_ref().map_or(true, |b| ver > b.0) {
            best = Some((ver, tag.trim_start_matches('v').to_string(), url.to_string(), asset["size"].as_u64().unwrap_or(0)));
        }
    }
    let Some((_, version, url, size)) = best else { return Ok(None) };
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
    Ok(Some((version, ready)))
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
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| format!("не удалось заменить файл программы: {e}"))?;
    if let Err(e) = std::fs::copy(new, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("не удалось поставить новую версию: {e}"));
    }
    match std::process::Command::new(&exe).arg("--updated").env_remove("OSTROVOK_FAKE_VERSION").spawn() {
        Ok(_) => Ok(()),
        Err(e) => {
            // новая версия не запустилась (например, её остановил антивирус) — возвращаем прежнюю
            let _ = std::fs::remove_file(&exe);
            let _ = std::fs::rename(&old, &exe);
            Err(format!("новая версия не запустилась: {e}"))
        }
    }
}

/// Убирает следы прошлого обновления: старый файл программы и скачанные установщики.
pub fn cleanup() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(6));
        if let Some(old) = old_exe() {
            let _ = std::fs::remove_file(old);
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
}
