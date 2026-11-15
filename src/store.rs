//! Настройки и заметки в %APPDATA%\Ostrovok, автозапуск.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Settings {
    #[serde(default)]
    pub city: String,
    #[serde(default)]
    pub city_name: String,
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
pub struct Reminder {
    pub text: String,
    /// время срабатывания, секунды с 1970 года
    pub at: i64,
}

pub fn dir() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("Ostrovok")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(dir().join("settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let _ = std::fs::create_dir_all(dir());
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(dir().join("settings.json"), s);
        }
    }
}

pub fn load_notes() -> String {
    std::fs::read_to_string(dir().join("notes.txt")).unwrap_or_default()
}

pub fn save_notes(text: &str) {
    let _ = std::fs::create_dir_all(dir());
    let _ = std::fs::write(dir().join("notes.txt"), text);
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
