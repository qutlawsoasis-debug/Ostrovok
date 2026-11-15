//! «Джарвис»: помощник на локальной модели. Модель крутит отдельный процесс llama-server (llama.cpp),
//! остров говорит с ним по HTTP на 127.0.0.1. Наружу ничего не уходит.
//! Модель сама решает, какие «руки» (инструменты) вызвать; остров их выполняет и возвращает результат.

use serde_json::{json, Value};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const NO_WINDOW: u32 = 0x0800_0000;
/// сколько держать модель в памяти после последнего обращения
const IDLE: Duration = Duration::from_secs(90);
const MAX_ROUNDS: usize = 6;

/// Размеры модели: ключ, название, имя файла.
pub const MODELS: [(&str, &str, &str); 3] =
    [("small", "Малая", "small.gguf"), ("medium", "Средняя", "medium.gguf"), ("large", "Большая", "large.gguf")];

/// Облачный мозг: адрес сервера с протоколом чата OpenAI (свой посредник или сам сервис), ключ и имя модели.
/// Ключ и модель могут быть пустыми — посредник подставляет свои.
#[derive(Clone)]
pub struct Remote {
    pub url: String,
    pub key: String,
    pub model: String,
}

pub enum Ev {
    /// что сейчас происходит («Загружаю модель…»)
    Status(String),
    /// модель вызвала действие — строка для показа
    Step(String),
    /// действие, которому нужно состояние острова: выполняет поток интерфейса и отвечает в канал
    Tool(String, Value, mpsc::Sender<String>),
    /// необратимое действие: ждём «да» или «нет»
    Confirm(String, mpsc::Sender<bool>),
    /// итог: текст ответа, переписка для следующего вопроса, строка со скоростью и где думал
    Done(Result<String, String>, Vec<Value>, String),
}

struct Server {
    child: Child,
    port: u16,
    model: String,
    gpu: bool,
    used: Instant,
    /// закрытие задания Windows убивает сервер, даже если остров упал
    _job: isize,
}

#[derive(Clone)]
pub struct Jarvis {
    server: Arc<Mutex<Option<Server>>>,
    loading: Arc<Mutex<()>>,
    dir: PathBuf,
}

impl Jarvis {
    pub fn new(dir: PathBuf) -> Self {
        let j = Jarvis { server: Arc::new(Mutex::new(None)), loading: Arc::new(Mutex::new(())), dir };
        // выгружаем модель, когда к ней давно не обращались
        let server = j.server.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            let mut s = server.lock().unwrap();
            if s.as_ref().is_some_and(|s| s.used.elapsed() > IDLE) {
                if let Some(mut old) = s.take() {
                    let _ = old.child.kill();
                }
            }
        });
        j
    }

    pub fn model_path(&self, model: &str) -> PathBuf {
        let file = MODELS.iter().find(|m| m.0 == model).map_or("large.gguf", |m| m.2);
        self.dir.join("models").join(file)
    }

    pub fn loaded(&self) -> Option<String> {
        self.server.lock().unwrap().as_ref().map(|s| s.model.clone())
    }

    pub fn stop(&self) {
        if let Some(mut s) = self.server.lock().unwrap().take() {
            let _ = s.child.kill();
        }
    }

    /// Поднимает сервер с нужной моделью (или берёт уже работающий) и возвращает порт.
    fn ensure(&self, model: &str, gpu: bool, emit: &dyn Fn(Ev)) -> Result<u16, String> {
        let _one = self.loading.lock().unwrap();
        {
            let mut s = self.server.lock().unwrap();
            if let Some(cur) = s.as_mut() {
                if cur.model == model && cur.gpu == gpu && cur.child.try_wait().ok().flatten().is_none() {
                    cur.used = Instant::now();
                    return Ok(cur.port);
                }
            }
            if let Some(mut old) = s.take() {
                let _ = old.child.kill();
            }
        }
        let exe = self.dir.join("engine").join("llama-server.exe");
        let path = self.model_path(model);
        if !exe.exists() {
            return Err(format!("Не найден движок: {}", exe.display()));
        }
        if !path.exists() {
            return Err("Эта модель ещё не скачана".into());
        }
        emit(Ev::Status("Загружаю модель в память…".into()));
        let port = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map(|a| a.port()).map_err(|e| e.to_string())?;
        let threads = std::thread::available_parallelism().map_or(4, |n| (n.get() / 2).max(2));
        let child = Command::new(&exe)
            .arg("-m").arg(&path)
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["-c", "8192", "--jinja", "--reasoning-budget", "0", "--no-webui"])
            .args(["-t", &threads.to_string()])
            .args(["-ngl", if gpu { "99" } else { "0" }])
            .current_dir(self.dir.join("engine"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(NO_WINDOW)
            .spawn()
            .map_err(|e| format!("Движок не запустился: {e}"))?;
        let job = tie_to_us(&child);
        let mut srv = Server { child, port, model: model.to_string(), gpu, used: Instant::now(), _job: job };
        let start = Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(250));
            if srv.child.try_wait().ok().flatten().is_some() {
                return Err("Движок закрылся при загрузке модели (не хватило памяти?)".into());
            }
            let ok = ureq::get(&format!("http://127.0.0.1:{port}/health")).timeout(Duration::from_secs(2)).call().is_ok_and(|r| r.status() == 200);
            if ok {
                break;
            }
            if start.elapsed() > Duration::from_secs(120) {
                let _ = srv.child.kill();
                return Err("Модель слишком долго загружается".into());
            }
        }
        srv.used = Instant::now();
        *self.server.lock().unwrap() = Some(srv);
        Ok(port)
    }

    /// Заранее грузит модель в память — пока человек ещё говорит.
    pub fn warm(&self, model: String, gpu: bool) {
        let me = self.clone();
        std::thread::spawn(move || {
            let _ = me.ensure(&model, gpu, &|_| {});
        });
    }

    fn touch(&self) {
        if let Some(s) = self.server.lock().unwrap().as_mut() {
            s.used = Instant::now();
        }
    }

    /// Вопрос пользователя: думает в отдельном потоке, о ходе сообщает через emit.
    /// history — переписка в формате чата (без системного сообщения), последним идёт новый вопрос.
    pub fn ask(&self, history: Vec<Value>, model: String, gpu: bool, remote: Option<Remote>, emit: impl Fn(Ev) + Send + 'static) {
        let me = self.clone();
        std::thread::spawn(move || {
            let mut msgs = history;
            let started = Instant::now();
            let mut speed = 0.0;
            let mut place = "на компьютере";
            let res = me.run(&mut msgs, &model, gpu, remote, &emit, &mut speed, &mut place);
            let stats = format!("ответ {:.1} с ({place})", started.elapsed().as_secs_f32());
            let _ = speed;
            emit(Ev::Done(res, msgs, stats));
        });
    }

    /// Один запрос к облачному мозгу. Err — сервер недоступен или отказал: тогда думаем сами.
    fn chat_remote(r: &Remote, all: &[Value], tools: &Value) -> Result<Value, String> {
        let mut body = json!({ "messages": all, "tools": tools, "temperature": 0.4, "max_tokens": 600 });
        if !r.model.is_empty() {
            body["model"] = json!(r.model);
        }
        let mut req = ureq::post(&format!("{}/chat/completions", r.url.trim_end_matches('/'))).timeout(Duration::from_secs(25));
        if !r.key.is_empty() {
            req = req.set("Authorization", &format!("Bearer {}", r.key));
        }
        req.send_json(body)
            .map_err(|e| match e {
                ureq::Error::Status(c, r) => format!("сервер ответил {c}: {}", r.into_string().unwrap_or_default().chars().take(200).collect::<String>()),
                _ => "нет связи с сервером".to_string(),
            })?
            .into_json()
            .map_err(|e| e.to_string())
    }

    fn chat_local(&self, port: u16, all: &[Value], tools: &Value) -> Result<Value, String> {
        let body = json!({ "messages": all, "tools": tools, "temperature": 0.4, "max_tokens": 600,
            // без «размышлений вслух»: они съедают секунды и не нужны для коротких просьб
            "chat_template_kwargs": { "enable_thinking": false } });
        self.touch();
        ureq::post(&format!("http://127.0.0.1:{port}/v1/chat/completions"))
            .timeout(Duration::from_secs(300))
            .send_json(body)
            .map_err(|e| match e {
                ureq::Error::Status(c, r) => format!("Движок ответил ошибкой {c}: {}", r.into_string().unwrap_or_default().chars().take(160).collect::<String>()),
                _ => "Движок не отвечает".to_string(),
            })?
            .into_json()
            .map_err(|e| e.to_string())
    }

    #[allow(clippy::too_many_arguments)]
    fn run(&self, msgs: &mut Vec<Value>, model: &str, gpu: bool, mut remote: Option<Remote>, emit: &dyn Fn(Ev), speed: &mut f64, place: &mut &'static str) -> Result<String, String> {
        let tools = tools();
        let mut port = None;
        for _ in 0..MAX_ROUNDS {
            emit(Ev::Status("Думаю…".into()));
            let mut all = vec![json!({ "role": "system", "content": system_prompt() })];
            all.extend(msgs.iter().cloned());
            let mut answer = None;
            if let Some(r) = &remote {
                match Self::chat_remote(r, &all, &tools) {
                    Ok(v) => {
                        *place = "в облаке";
                        answer = Some(v);
                    }
                    Err(e) => {
                        // облако не ответило — если есть локальная модель, думаем сами
                        if !self.model_path(model).exists() {
                            return Err(format!("Облачный мозг недоступен: {e}"));
                        }
                        emit(Ev::Step(format!("Облако недоступно ({e}) — думаю на компьютере")));
                        remote = None;
                    }
                }
            }
            let v: Value = match answer {
                Some(v) => v,
                None => {
                    let p = match port {
                        Some(p) => p,
                        None => {
                            let p = self.ensure(model, gpu, emit)?;
                            emit(Ev::Status("Думаю…".into()));
                            port = Some(p);
                            p
                        }
                    };
                    *place = "на компьютере";
                    self.chat_local(p, &all, &tools)?
                }
            };
            self.touch();
            if let Some(s) = v["timings"]["predicted_per_second"].as_f64() {
                *speed = s;
            }
            let m = &v["choices"][0]["message"];
            let calls = m["tool_calls"].as_array().cloned().unwrap_or_default();
            if calls.is_empty() {
                let text = clean(m["content"].as_str().unwrap_or(""));
                if text.is_empty() {
                    return Err("Модель ничего не ответила".into());
                }
                msgs.push(json!({ "role": "assistant", "content": text }));
                return Ok(text);
            }
            msgs.push(json!({ "role": "assistant", "content": m["content"].as_str().unwrap_or(""), "tool_calls": calls }));
            for c in &calls {
                let name = c["function"]["name"].as_str().unwrap_or("").to_string();
                let args: Value = match &c["function"]["arguments"] {
                    Value::String(s) => serde_json::from_str(s).unwrap_or(json!({})),
                    other => other.clone(),
                };
                emit(Ev::Step(describe(&name, &args)));
                let out = self.call(&name, &args, emit);
                let out: String = out.chars().take(3000).collect();
                msgs.push(json!({ "role": "tool", "tool_call_id": c["id"].as_str().unwrap_or("0"), "content": out }));
            }
        }
        Err("Слишком длинная цепочка действий — остановился".into())
    }

    /// Выполняет одно действие и возвращает модели результат текстом.
    fn call(&self, name: &str, args: &Value, emit: &dyn Fn(Ev)) -> String {
        let s = |k: &str| args[k].as_str().unwrap_or("").trim().to_string();
        match name {
            "read_screen" => match crate::sys::capture_screen().and_then(|(w, h, px)| crate::sys::ocr(w, h, &px)) {
                Ok(t) if !t.trim().is_empty() => format!("Текст на экране:\n{t}"),
                Ok(_) => "На экране не найдено текста".into(),
                Err(e) => format!("Ошибка: {e}"),
            },
            "find_files" => find_files(&s("query")),
            "run_command" => {
                let cmd = s("command");
                if cmd.is_empty() {
                    return "Ошибка: пустая команда".into();
                }
                if !confirm(emit, format!("Выполнить команду?\n{cmd}")) {
                    return "Пользователь запретил выполнять эту команду".into();
                }
                run_powershell(&cmd)
            }
            "power" => {
                let a = s("action");
                let label = match a.as_str() {
                    "lock" => "Заблокировать компьютер?",
                    "sleep" => "Перевести компьютер в сон?",
                    "restart" => "Перезагрузить компьютер?",
                    "off" => "Выключить компьютер?",
                    _ => return "Ошибка: action должен быть lock, sleep, restart или off".into(),
                };
                if a != "lock" && !confirm(emit, label.into()) {
                    return "Пользователь отказался".into();
                }
                crate::sys::power(&a);
                "Готово".into()
            }
            _ => {
                // остальным нужно состояние острова — выполняет поток интерфейса
                let (tx, rx) = mpsc::channel();
                emit(Ev::Tool(name.to_string(), args.clone(), tx));
                rx.recv_timeout(Duration::from_secs(20)).unwrap_or_else(|_| "Ошибка: остров не ответил".into())
            }
        }
    }
}

fn confirm(emit: &dyn Fn(Ev), text: String) -> bool {
    let (tx, rx) = mpsc::channel();
    emit(Ev::Confirm(text, tx));
    rx.recv_timeout(Duration::from_secs(120)).unwrap_or(false)
}

/// Убирает размышления модели и разметку, которую панель не показывает.
fn clean(text: &str) -> String {
    let mut t = text.to_string();
    // размышление без открывающей метки: всё до закрывающей — не ответ
    if let Some(b) = t.rfind("</think>") {
        if !t[..b].contains("<think>") {
            t = t[b + 8..].to_string();
        }
    }
    while let (Some(a), Some(b)) = (t.find("<think>"), t.find("</think>")) {
        if b < a {
            break;
        }
        t.replace_range(a..b + 8, "");
    }
    t.replace("**", "").replace("`", "").trim().to_string()
}

fn system_prompt() -> String {
    let home = std::env::var("USERPROFILE").unwrap_or_default();
    format!(
        "Тебя зовут Джарвис, ты голосовой помощник, встроенный в компьютер с Windows. Пользователь — твой хозяин; его зовут не Джарвис, не называй его так. Ты управляешь этим компьютером с помощью инструментов.\n\
Правила:\n\
- Если просят что-то сделать на компьютере — вызови подходящий инструмент. Никогда не говори, что сделал, если не вызывал инструмент.\n\
- Если нужно несколько действий — вызывай инструменты по очереди.\n\
- Если инструмент ответил, что всё сделано, — не повторяй его и не вызывай другие, просто коротко скажи, что готово.\n\
- Если подходящего инструмента нет, попробуй run_command (PowerShell). Если и так нельзя — честно скажи, что не умеешь.\n\
- На обычные вопросы и разговор отвечай сам, без инструментов: шутку расскажи, на вопрос ответь по существу. Не здоровайся, если с тобой не здоровались. Время и дату узнавай через system_info.\n\
- Речь распознаётся с ошибками: если слово искажено, догадайся по смыслу, что имелось в виду.\n\
- Отвечай по-русски, коротко, одним-двумя предложениями, обычным текстом без разметки, списков и смайликов: ответ будет произнесён вслух.\n\
Папка пользователя: {home}. Рабочий стол: {home}\\Desktop, загрузки: {home}\\Downloads, документы: {home}\\Documents.",
    )
}

fn tool(name: &str, about: &str, props: Value, required: &[&str]) -> Value {
    json!({ "type": "function", "function": { "name": name, "description": about,
        "parameters": { "type": "object", "properties": props, "required": required } } })
}

fn tools() -> Value {
    json!([
        tool("open", "Открыть программу, сайт, файл или папку по названию. Примеры target: «блокнот», «steam», «гугл», «youtube.com», «загрузки», «C:\\Users\\file.txt». Чтобы открыть сайт — используй этот инструмент, а не поиск.",
            json!({ "target": { "type": "string", "description": "название программы, адрес сайта или путь" } }), &["target"]),
        tool("web_search", "Найти информацию в интернете: открывает в браузере поиск по запросу. Только когда просят что-то найти или загуглить.", json!({ "query": { "type": "string" } }), &["query"]),
        tool("set_volume", "Установить громкость звука компьютера.", json!({ "percent": { "type": "integer", "description": "от 0 до 100" } }), &["percent"]),
        tool("media", "Управление музыкой и звуком.",
            json!({ "action": { "type": "string", "enum": ["play_pause", "next", "previous", "mute", "unmute"] } }), &["action"]),
        tool("timer", "Запустить таймер обратного отсчёта.", json!({ "minutes": { "type": "number" } }), &["minutes"]),
        tool("remind", "Создать напоминание.",
            json!({ "text": { "type": "string", "description": "о чём напомнить" },
                    "time": { "type": "string", "description": "время «18:30» или через сколько минут «+15»" } }), &["text", "time"]),
        tool("clipboard_read", "Прочитать текст из буфера обмена.", json!({}), &[]),
        tool("clipboard_write", "Положить текст в буфер обмена.", json!({ "text": { "type": "string" } }), &["text"]),
        tool("note_add", "Дописать строку в заметки пользователя.", json!({ "text": { "type": "string" } }), &["text"]),
        tool("screenshot", "Сделать снимок всего экрана и сохранить в файл.", json!({}), &[]),
        tool("read_screen", "Прочитать весь текст, который сейчас виден на экране.", json!({}), &[]),
        tool("system_info", "Узнать текущее время и дату, а также состояние компьютера: загрузка процессора, память, диск, батарея, громкость, погода, что играет.", json!({}), &[]),
        tool("find_files", "Найти файлы по части имени в папках пользователя. Возвращает пути, сначала самые новые.",
            json!({ "query": { "type": "string", "description": "часть имени файла или расширение, например «отчёт» или «.png»" } }), &["query"]),
        tool("run_command", "Выполнить любую команду PowerShell и получить её вывод. Для всего, что не умеют другие инструменты: файлы, процессы, настройки.",
            json!({ "command": { "type": "string" } }), &["command"]),
        tool("power", "Заблокировать, усыпить, перезагрузить или выключить компьютер.",
            json!({ "action": { "type": "string", "enum": ["lock", "sleep", "restart", "off"] } }), &["action"]),
    ])
}

/// Строка «что делает», которую видит пользователь.
fn describe(name: &str, a: &Value) -> String {
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    match name {
        "open" => format!("Открываю: {}", s("target")),
        "web_search" => format!("Ищу в интернете: {}", s("query")),
        "set_volume" => format!("Громкость: {}%", a["percent"].as_f64().unwrap_or(0.0)),
        "media" => format!("Музыка: {}", s("action")),
        "timer" => format!("Таймер: {} мин", a["minutes"].as_f64().unwrap_or(0.0)),
        "remind" => format!("Напоминание: {} ({})", s("text"), s("time")),
        "clipboard_read" => "Читаю буфер обмена".into(),
        "clipboard_write" => "Кладу текст в буфер".into(),
        "note_add" => format!("Заметка: {}", s("text")),
        "screenshot" => "Снимаю экран".into(),
        "read_screen" => "Читаю текст с экрана".into(),
        "system_info" => "Смотрю состояние компьютера".into(),
        "find_files" => format!("Ищу файлы: {}", s("query")),
        "run_command" => format!("Команда: {}", s("command")),
        "power" => format!("Питание: {}", s("action")),
        other => format!("Неизвестное действие: {other}"),
    }
}

fn run_powershell(cmd: &str) -> String {
    let script = format!("[Console]::OutputEncoding=[Text.Encoding]::UTF8; {cmd}");
    let child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(NO_WINDOW)
        .spawn();
    let Ok(child) = child else { return "Ошибка: PowerShell не запустился".into() };
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(Duration::from_secs(40)) {
        Ok(Ok(o)) => {
            let mut out = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
            if !err.is_empty() {
                out = format!("{out}\nОшибка: {err}");
            }
            if out.trim().is_empty() { "Команда выполнена, вывода нет".into() } else { out }
        }
        _ => {
            let _ = Command::new("taskkill").args(["/F", "/T", "/PID", &pid.to_string()]).creation_flags(NO_WINDOW).status();
            "Ошибка: команда выполнялась дольше 40 секунд и была остановлена".into()
        }
    }
}

fn find_files(query: &str) -> String {
    let q = query.to_lowercase();
    if q.is_empty() {
        return "Ошибка: пустой запрос".into();
    }
    let home = PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default());
    let mut found: Vec<(std::time::SystemTime, String)> = Vec::new();
    fn walk(dir: &Path, depth: u8, q: &str, out: &mut Vec<(std::time::SystemTime, String)>, deadline: Instant) {
        if Instant::now() > deadline {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name.starts_with('.') || name == "node_modules" || name == "target" {
                continue;
            }
            if name.contains(q) {
                let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                out.push((t, p.display().to_string()));
            }
            if depth < 4 && p.is_dir() {
                walk(&p, depth + 1, q, out, deadline);
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    for sub in ["Desktop", "Downloads", "Documents", "Pictures", "Videos", "Music"] {
        walk(&home.join(sub), 0, &q, &mut found, deadline);
    }
    if found.is_empty() {
        return "Ничего не найдено".into();
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.iter().take(8).map(|f| f.1.clone()).collect::<Vec<_>>().join("\n")
}

/// Привязывает процесс сервера к острову: Windows убьёт его, когда остров завершится любым способом.
pub(crate) fn tie_to_us(child: &Child) -> isize {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::*;
    unsafe {
        let Ok(job) = CreateJobObjectW(None, PCWSTR::null()) else { return 0 };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let _ = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        let _ = AssignProcessToJobObject(job, HANDLE(child.as_raw_handle() as _));
        job.0 as isize
    }
}
