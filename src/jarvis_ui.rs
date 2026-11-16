//! Страница «Джарвис» и действия, которым нужно состояние острова.

use crate::ears::Heard;
use crate::jarvis::{Ev, MODELS};
use crate::{empty_state, glass_chip, jelly, semi, store, sys, weather, App, Mode, Msg, Tab, ACCENT, DIM, FG};
use eframe::egui::{self, vec2, Align2, Color32, FontId, RichText, Sense};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// Похоже ли слово на «Джарвис». Русская модель распознавания этого имени не знает и пишет его
/// как слышит: Чарлиз, Зарвес, Джаргас, Jarvis. Сравниваем по звучанию, а не по буквам.
fn is_name(word: &str) -> bool {
    let w = word.to_lowercase();
    if !(5..=8).contains(&w.chars().count()) {
        return false;
    }
    if w.contains("jarv") || w.contains("jerv") {
        return true;
    }
    // начало слова: любой шипящий или свистящий звук → «ж»
    let mut rest = w.as_str();
    for head in ["дж", "дьж", "дз", "тч", "ж", "ч", "з"] {
        if let Some(r) = w.strip_prefix(head) {
            rest = r;
            break;
        }
    }
    if rest.len() == w.len() {
        return false;
    }
    // дальше — похожие звуки к одному виду
    let sound: String = std::iter::once('ж')
        .chain(rest.chars().map(|c| match c {
            'л' | 'б' | 'г' => 'в',
            'з' | 'ц' => 'с',
            'е' | 'э' | 'ы' => 'и',
            'о' | 'я' => 'а',
            other => other,
        }))
        .collect();
    distance(&sound, "жарвис") <= 1
}

/// Сколько букв надо заменить, вставить или убрать, чтобы из одного слова получить другое.
fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut prev = row[0];
        row[0] = i;
        for j in 1..=b.len() {
            let cur = row[j];
            row[j] = (prev + (a[i - 1] != b[j - 1]) as usize).min(row[j] + 1).min(row[j - 1] + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// Ищет обращение по имени в любом месте фразы. Возвращает, было ли оно, и фразу без него.
fn split_name(text: &str) -> (bool, String) {
    let bare = |w: &str| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    let words: Vec<&str> = text.split_whitespace().collect();
    let kept: Vec<&str> = words.iter().copied().filter(|w| !is_name(&bare(w))).collect();
    let rest = kept.join(" ").trim_matches(|c: char| ",.!?".contains(c) || c.is_whitespace()).to_string();
    (kept.len() != words.len(), rest)
}

pub fn default_dir(cfg: &store::Settings) -> std::path::PathBuf {
    // для проверок: другая папка без правки настроек
    if let Ok(d) = std::env::var("OSTROVOK_JARVIS_DIR") {
        return d.into();
    }
    if cfg.jarvis_dir.trim().is_empty() { store::dir().join("jarvis") } else { cfg.jarvis_dir.trim().into() }
}

impl App {
    fn jv_model(&self) -> String {
        if MODELS.iter().any(|m| m.0 == self.cfg.jarvis_model) { self.cfg.jarvis_model.clone() } else { ["large", "medium", "small"].into_iter().find(|m| self.jarvis.model_path(m).exists()).unwrap_or("large").into() }
    }

    fn jv_remote(&self) -> Option<crate::jarvis::Remote> {
        let url = self.cfg.jarvis_url.trim();
        (!url.is_empty()).then(|| crate::jarvis::Remote { url: url.to_string(), key: self.cfg.jarvis_key.trim().to_string(), model: self.cfg.jarvis_remote_model.trim().to_string() })
    }

    pub(crate) fn jarvis_ask(&mut self, question: String, ctx: &egui::Context) {
        let q = question.trim().to_string();
        if q.is_empty() || self.jv_busy {
            return;
        }
        self.jv_log.push((0, q.clone()));
        self.jv_msgs.push(json!({ "role": "user", "content": q }));
        // короткая память: маленькие модели путаются в длинной переписке
        while self.jv_msgs.len() > 16 {
            self.jv_msgs.remove(0);
        }
        while self.jv_msgs.first().is_some_and(|m| m["role"] != "user") {
            self.jv_msgs.remove(0);
        }
        self.voice.hush();
        self.jv_busy = true;
        self.jv_status = "Думаю…".into();
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        self.jarvis.ask(self.jv_msgs.clone(), self.jv_model(), !self.cfg.jarvis_cpu, self.jv_remote(), move |ev| {
            let _ = tx.send(Msg::Jarvis(ev));
            ctx.request_repaint();
        });
    }

    fn jarvis_listen(&mut self, ctx: &egui::Context) {
        if self.jv_listen != 0 {
            // повторное нажатие — закончить фразу сейчас
            self.ears.finish();
            return;
        }
        if self.jv_busy {
            return;
        }
        if !self.ears.ready() {
            self.jv_log.push((3, "Распознавание речи ещё не скачано".into()));
            return;
        }
        self.voice.hush();
        self.jv_listen = 1;
        self.jv_level = 0.0;
        // пока человек говорит, модель и распознаватель уже грузятся
        self.ears.warm();
        // с облачным мозгом локальную модель заранее не грузим — память остаётся свободной
        if self.jv_remote().is_none() {
            self.jarvis.warm(self.jv_model(), !self.cfg.jarvis_cpu);
        }
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        self.ears.listen(move |h| {
            let _ = tx.send(Msg::Heard(h));
            ctx.request_repaint();
        });
    }

    pub(crate) fn jarvis_heard(&mut self, h: Heard, ctx: &egui::Context) {
        match h {
            Heard::Level(l) => self.jv_level = l,
            Heard::Thinking => self.jv_listen = 2,
            Heard::Text(t, secs, engine) => {
                self.jv_listen = 0;
                self.jv_heard = secs;
                self.jv_engine = engine;
                // само имя в начале фразы — обращение, а не часть просьбы
                let (_, rest) = split_name(&t);
                if rest.chars().filter(|c| c.is_alphanumeric()).count() < 2 {
                    // позвали по имени и замолчали — слушаем дальше
                    self.jv_log.push((1, "Слушаю.".into()));
                    self.jarvis_listen(ctx);
                } else if self.jv_log.last().is_some_and(|l| l.0 == 1 && l.1 == "Слушаю.") {
                    self.jv_log.pop();
                    self.jarvis_ask(rest, ctx);
                } else {
                    self.jarvis_ask(rest, ctx);
                }
            }
            Heard::Error(e) => {
                self.jv_listen = 0;
                self.jv_log.push((3, e));
            }
            Heard::Phrase(t) => {
                // пока идёт запись по кнопке или Джарвис думает — посторонние фразы не нужны
                if self.jv_listen != 0 || self.jv_busy {
                    return;
                }
                let (named, rest) = split_name(&t);
                let attentive = self.jv_attn.is_some_and(|until| Instant::now() < until);
                let empty = rest.chars().filter(|c| c.is_alphanumeric()).count() < 2;
                if named && empty {
                    // позвали и замолчали: ждём просьбу следующей фразой
                    self.jv_attn = Some(Instant::now() + Duration::from_secs(8));
                    self.show_toast("\u{E720}", [0xB9, 0xA4, 0xFF], "Джарвис", "Слушаю", 8, false);
                } else if (named || attentive) && !empty {
                    self.jv_attn = None;
                    self.show_toast("\u{E720}", [0xB9, 0xA4, 0xFF], "Джарвис", &rest, 5, false);
                    self.jarvis_ask(rest, ctx);
                }
                // остальные разговоры рядом с микрофоном никуда не пишутся
            }
        }
    }

    pub(crate) fn jarvis_event(&mut self, ev: Ev) {
        match ev {
            Ev::Status(s) => self.jv_status = s,
            Ev::Step(s) => self.jv_log.push((2, s)),
            Ev::Tool(name, args, reply) => {
                let out = self.jarvis_tool(&name, &args);
                let _ = reply.send(out);
            }
            Ev::Confirm(text, reply) => {
                self.jv_confirm = Some((text, reply));
                self.mode = Mode::Panel;
                self.tab = Tab::Jarvis;
                self.sticky = true;
            }
            Ev::Done(res, msgs, stats) => {
                self.jv_busy = false;
                self.jv_status.clear();
                self.jv_confirm = None;
                self.jv_msgs = msgs;
                self.jv_stats = stats;
                match res {
                    Ok(text) => {
                        if !self.cfg.jarvis_mute {
                            let cloud = (!self.cfg.fish_key.trim().is_empty() && !self.cfg.fish_voice.trim().is_empty())
                                .then(|| (self.cfg.fish_key.trim().to_string(), self.cfg.fish_voice.trim().to_string()));
                            self.voice.say(&text, cloud);
                        }
                        if !(self.mode == Mode::Panel && self.tab == Tab::Jarvis) {
                            self.show_toast("\u{E99A}", [0xB9, 0xA4, 0xFF], "Джарвис", &text, 7, false);
                        }
                        self.jv_log.push((1, text));
                    }
                    Err(e) => self.jv_log.push((3, e)),
                }
            }
        }
    }

    /// Действия Джарвиса над самим островом и системой. Возвращает модели короткий итог.
    fn jarvis_tool(&mut self, name: &str, a: &Value) -> String {
        let s = |k: &str| a[k].as_str().unwrap_or("").trim().to_string();
        match name {
            "open" => self.jv_open(&s("target")),
            "web_search" => {
                let q = s("query");
                if q.is_empty() {
                    return "Ошибка: пустой запрос".into();
                }
                sys::open(&format!("https://www.google.com/search?q={}", weather::encode(&q)));
                "Открыл поиск в браузере".into()
            }
            "set_volume" => match a["percent"].as_f64().or_else(|| a["percent"].as_str().and_then(|v| v.parse().ok())) {
                Some(p) => {
                    let p = p.clamp(0.0, 100.0);
                    sys::set_volume(p as f32 / 100.0);
                    sys::set_mute(false);
                    self.vol = sys::volume();
                    format!("Громкость теперь {p:.0}%")
                }
                None => "Ошибка: нужен percent от 0 до 100".into(),
            },
            "media" => {
                match s("action").as_str() {
                    "play_pause" => sys::press_key(0xB3),
                    "next" => sys::press_key(0xB0),
                    "previous" => sys::press_key(0xB1),
                    "mute" => sys::set_mute(true),
                    "unmute" => sys::set_mute(false),
                    _ => return "Ошибка: неизвестный action".into(),
                }
                "Готово".into()
            }
            "timer" => match a["minutes"].as_f64().or_else(|| a["minutes"].as_str().and_then(|v| v.parse().ok())) {
                Some(m) if m > 0.0 && m <= 24.0 * 60.0 => {
                    self.start_timer(m);
                    format!("Таймер запущен на {m} мин")
                }
                _ => "Ошибка: minutes должно быть числом больше нуля".into(),
            },
            "remind" => {
                let before = self.cfg.reminders.len();
                self.remind_text = s("text");
                self.remind_time = s("time");
                self.add_reminder();
                if self.cfg.reminders.len() > before {
                    "Напоминание создано".into()
                } else {
                    self.remind_text.clear();
                    self.remind_time.clear();
                    "Ошибка: время должно быть в виде «18:30» или «+15» (через сколько минут)".into()
                }
            }
            "clipboard_read" => match self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()) {
                Some(t) if !t.trim().is_empty() => format!("В буфере обмена:\n{}", t.chars().take(2000).collect::<String>()),
                _ => "В буфере обмена нет текста".into(),
            },
            "clipboard_write" => {
                self.set_clipboard(&s("text"));
                "Текст в буфере обмена".into()
            }
            "note_add" => {
                let t = s("text");
                if t.is_empty() {
                    return "Ошибка: пустая заметка".into();
                }
                if !self.notes.is_empty() && !self.notes.ends_with('\n') {
                    self.notes.push('\n');
                }
                self.notes.push_str(&t);
                self.notes_dirty = Some(Instant::now());
                "Записал в заметки".into()
            }
            "screenshot" => {
                self.mode = Mode::Pill;
                self.shot = Some(6);
                "Снимок экрана сохранён в папку Изображения\\Островок и скопирован в буфер".into()
            }
            "system_info" => {
                let st = &self.stats;
                const DAYS: [&str; 7] = ["понедельник", "вторник", "среда", "четверг", "пятница", "суббота", "воскресенье"];
                use chrono::Datelike;
                let now = chrono::Local::now();
                let when = format!("Сейчас {}, {}, {}. ", now.format("%H:%M"), DAYS[now.weekday().num_days_from_monday() as usize], now.format("%d.%m.%Y"));
                let mut out = when + &format!(
                    "Процессор загружен на {:.0}%. Память: занято {:.1} из {:.0} ГБ. Диск C: свободно {:.0} из {:.0} ГБ.",
                    st.cpu * 100.0,
                    st.mem_used,
                    st.mem_total,
                    st.disk_free,
                    st.disk_total
                );
                if let Some((pct, plugged)) = st.battery {
                    out += &format!(" Батарея {pct}%{}.", if plugged { ", заряжается" } else { "" });
                }
                if let Some((v, muted)) = sys::volume() {
                    out += &format!(" Громкость {:.0}%{}.", v * 100.0, if muted { ", звук выключен" } else { "" });
                }
                if let Some(w) = &self.weather {
                    out += &format!(" Погода в городе {}: {}, {}.", w.city, weather::deg(w.temp), weather::describe(w.code).1);
                }
                match &self.media_now {
                    Some(m) if m.playing => out += &format!(" Сейчас играет: {} — {}.", m.artist, m.title),
                    _ => out += " Музыка не играет.",
                }
                out
            }
            other => format!("Ошибка: инструмента {other} не существует"),
        }
    }

    fn jv_open(&mut self, target: &str) -> String {
        let t = target.trim().trim_matches('"');
        if t.is_empty() {
            return "Ошибка: не указано, что открыть".into();
        }
        let low = t.to_lowercase();
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        // привычные папки по-русски
        let folder = match low.as_str() {
            "загрузки" | "папка загрузки" | "downloads" => Some("Downloads"),
            "рабочий стол" | "desktop" => Some("Desktop"),
            "документы" | "мои документы" | "documents" => Some("Documents"),
            "изображения" | "картинки" | "pictures" => Some("Pictures"),
            "видео" | "videos" => Some("Videos"),
            "музыка" | "music" => Some("Music"),
            _ => None,
        };
        if let Some(f) = folder {
            sys::open(&format!(r"{home}\{f}"));
            return format!("Открыл папку {f}");
        }
        if low.starts_with("http://") || low.starts_with("https://") {
            sys::open(t);
            return "Открыл сайт".into();
        }
        if std::path::Path::new(t).exists() {
            sys::open(t);
            return "Открыл".into();
        }
        // установленные программы — по ярлыкам из меню «Пуск»
        let apps = self.apps.lock().unwrap().clone();
        let hit = apps
            .iter()
            .find(|(n, _)| n.to_lowercase() == low)
            .or_else(|| apps.iter().filter(|(n, _)| n.to_lowercase().contains(&low)).min_by_key(|(n, _)| n.len()))
            .or_else(|| apps.iter().filter(|(n, _)| n.len() > 3 && low.contains(&n.to_lowercase())).max_by_key(|(n, _)| n.len()));
        if let Some((n, p)) = hit {
            sys::open(p);
            return format!("Запустил программу {n}");
        }
        // системные программы по-русски
        let builtin = match low.as_str() {
            "блокнот" => Some("notepad.exe"),
            "калькулятор" => Some("calc.exe"),
            "проводник" => Some("explorer.exe"),
            "диспетчер задач" => Some("taskmgr.exe"),
            "параметры" | "настройки" | "настройки windows" => Some("ms-settings:"),
            "командная строка" | "терминал" => Some("cmd.exe"),
            "paint" | "пейнт" => Some("mspaint.exe"),
            _ => None,
        };
        if let Some(b) = builtin {
            sys::open(b);
            return format!("Запустил: {t}");
        }
        // сайты, которые называют по имени
        let site = match low.as_str() {
            "гугл" | "google" => Some("google.com"),
            "ютуб" | "ютюб" | "youtube" => Some("youtube.com"),
            "вк" | "вконтакте" | "vk" => Some("vk.com"),
            "яндекс" | "yandex" => Some("ya.ru"),
            "твич" | "twitch" => Some("twitch.tv"),
            "гитхаб" | "github" => Some("github.com"),
            "википедия" | "wikipedia" => Some("ru.wikipedia.org"),
            "почта" | "gmail" | "гмейл" => Some("mail.google.com"),
            "переводчик" => Some("translate.google.com"),
            _ => None,
        };
        if let Some(u) = site {
            sys::open(&format!("https://{u}"));
            return format!("Открыл сайт {u}");
        }
        // одно латинское слово без точки — скорее всего сайт
        if t.len() > 2 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            sys::open(&format!("https://{low}.com"));
            return format!("Программы с таким именем нет, открыл сайт {low}.com");
        }
        // похоже на адрес сайта: одно слово с точкой
        if !t.contains(char::is_whitespace) && t.contains('.') && !t.contains('\\') {
            sys::open(&format!("https://{t}"));
            return "Открыл сайт".into();
        }
        format!("Ошибка: программа, файл или сайт «{t}» не найдены. Если это сайт — укажи адрес, например youtube.com")
    }

    /// Полоска загрузки с подписью.
    fn jv_progress(ui: &mut egui::Ui, prog: &crate::setup::Progress) {
        let (done, all) = prog.bytes;
        let frac = if all > 0 { (done as f32 / all as f32).clamp(0.0, 1.0) } else { 0.0 };
        let mb = |b: u64| b / 1_048_576;
        ui.label(RichText::new(format!("{}  ·  шаг {} из {}", prog.label, prog.step.0, prog.step.1)).color(FG));
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width().min(420.0), 8.0), Sense::hover());
        ui.painter().rect_filled(r, 4.0, Color32::from_white_alpha(22));
        ui.painter().rect_filled(egui::Rect::from_min_size(r.min, vec2(r.width() * frac.max(0.01), 8.0)), 4.0, ACCENT);
        ui.label(RichText::new(if all > 0 { format!("{} из {} МБ", mb(done), mb(all)) } else { format!("{} МБ", mb(done)) }).color(DIM).size(11.5));
    }

    fn jv_download(&mut self, model: &'static str, ctx: &egui::Context) {
        let ctx = ctx.clone();
        self.setup.start(model, move || ctx.request_repaint());
    }

    /// Первая установка: выбор модели и загрузка всего нужного.
    fn ui_jarvis_setup(&mut self, ui: &mut egui::Ui, prog: &crate::setup::Progress) {
        let ctx = ui.ctx().clone();
        ui.add_space(14.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Включить Джарвиса").font(semi(19.0)).color(FG));
            ui.add_space(2.0);
            ui.label(RichText::new("Голосовой помощник, который работает на этом компьютере: без ключей, подписок и интернета.").color(DIM));
            ui.label(RichText::new("Понимает просьбы своими словами и сам выполняет их: открывает программы, ставит таймеры, меняет громкость.").color(DIM));
        });
        ui.add_space(12.0);
        if prog.running {
            ui.vertical_centered(|ui| Self::jv_progress(ui, prog));
            ui.add_space(6.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("Можно закрыть панель — загрузка продолжится.").color(DIM).size(11.5)));
            return;
        }
        const CHOICES: [(&str, &str, &str); 3] = [
            ("large", "Большая — 2,4 ГБ", "Самая сообразительная. Нужно около 3 ГБ свободной памяти на время ответа."),
            ("medium", "Средняя — 1 ГБ", "Простые просьбы выполняет, в разговоре слабее."),
            ("small", "Малая — 0,4 ГБ", "Только для очень слабых компьютеров: часто отвечает невпопад."),
        ];
        for (id, name, note) in CHOICES {
            let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
            let on = self.jv_pick == id;
            glass_chip(ui.painter(), row, 11.0, if on { 0.9 } else if resp.hovered() { 0.6 } else { 0.35 });
            let p = ui.painter();
            p.circle_stroke(row.left_center() + vec2(20.0, 0.0), 6.0, egui::Stroke::new(1.5, if on { ACCENT } else { DIM }));
            if on {
                p.circle_filled(row.left_center() + vec2(20.0, 0.0), 3.2, ACCENT);
            }
            p.text(row.left_center() + vec2(38.0, 0.0), Align2::LEFT_CENTER, name, semi(13.5), FG);
            p.text(row.left_center() + vec2(190.0, 0.0), Align2::LEFT_CENTER, note, FontId::proportional(12.0), DIM);
            if resp.clicked() {
                self.jv_pick = id;
            }
            ui.add_space(3.0);
        }
        ui.add_space(6.0);
        if let Some(e) = &prog.error {
            ui.label(RichText::new(format!("Не получилось: {e}")).color(Color32::from_rgb(0xFF, 0x8A, 0x80)));
        }
        let need = self.setup.need_mb(self.jv_pick);
        let size = if need >= 1000 { format!("{:.1} ГБ", need as f32 / 1024.0).replace('.', ",") } else { format!("{need} МБ") };
        let (mut go, mut settings) = (false, false);
        ui.horizontal(|ui| {
            go = jelly(ui, if prog.error.is_some() { "Повторить".to_string() } else { format!("Скачать {size} и включить") }).clicked();
            settings = jelly(ui, "Настройки").clicked();
            ui.label(RichText::new("Вместе с моделью ставятся распознавание речи и голос.").color(DIM).size(11.5));
        });
        if go {
            self.jv_download(self.jv_pick, &ctx);
        }
        if settings {
            self.jv_view = 1;
        }
    }

    /// Настройки Джарвиса: модели, облачный мозг, голос, постоянное прослушивание.
    fn ui_jarvis_settings(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let prog = self.setup.progress();
        let mut back = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Настройки Джарвиса").font(semi(16.0)).color(FG));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| back = jelly(ui, "Назад").clicked());
        });
        if back {
            self.jv_view = 0;
        }
        let mut changed = false;
        let mut download = None;
        let field = |ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str, secret: bool| -> bool {
            let mut ch = false;
            ui.horizontal(|ui| {
                ui.add_sized([120.0, 20.0], egui::Label::new(RichText::new(label).color(DIM)));
                ch = ui
                    .add(egui::TextEdit::singleline(value).margin(egui::Margin::symmetric(9.0, 6.0)).password(secret).hint_text(RichText::new(hint).color(DIM)).desired_width(360.0))
                    .changed();
            });
            ch
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.label(RichText::new("Мозг на компьютере").font(semi(13.5)).color(ACCENT));
            for (id, name, size) in [("large", "Большая", "2,4 ГБ"), ("medium", "Средняя", "1 ГБ"), ("small", "Малая", "0,4 ГБ")] {
                ui.horizontal(|ui| {
                    ui.add_sized([120.0, 20.0], egui::Label::new(RichText::new(format!("{name} · {size}")).color(FG)));
                    if self.jarvis.model_path(id).exists() {
                        ui.label(RichText::new("установлена").color(Color32::from_rgb(0x7B, 0xE0, 0x9A)));
                    } else if ui.add_enabled(!prog.running, egui::Button::new("Скачать")).clicked() {
                        download = Some(id);
                    }
                });
            }
            if prog.running {
                Self::jv_progress(ui, &prog);
            } else if let Some(e) = &prog.error {
                ui.label(RichText::new(format!("Не получилось: {e}")).color(Color32::from_rgb(0xFF, 0x8A, 0x80)));
            }
            changed |= ui.checkbox(&mut self.cfg.jarvis_cpu, "Считать только на процессоре, без видеокарты").changed();
            ui.add_space(8.0);

            ui.label(RichText::new("Облачный мозг — необязательно").font(semi(13.5)).color(ACCENT));
            ui.label(RichText::new("Сервер с протоколом чата OpenAI (например, https://api.groq.com/openai/v1). Вопросы и ответы уходят на него, память компьютера не занята; если сервер недоступен, думает локальная модель.").color(DIM).size(11.5));
            changed |= field(ui, "Адрес", &mut self.cfg.jarvis_url, "пусто — думать на компьютере", false);
            changed |= field(ui, "Ключ", &mut self.cfg.jarvis_key, "своему посреднику не нужен", true);
            changed |= field(ui, "Модель", &mut self.cfg.jarvis_remote_model, "имя модели на сервере", false);
            ui.add_space(8.0);

            ui.label(RichText::new("Голос").font(semi(13.5)).color(ACCENT));
            let mut aloud = !self.cfg.jarvis_mute;
            if ui.checkbox(&mut aloud, "Отвечать вслух").changed() {
                self.cfg.jarvis_mute = !aloud;
                self.voice.hush();
                changed = true;
            }
            ui.label(RichText::new("Встроенный голос работает на компьютере. Свой голос Fish Audio — необязательно: тогда текст ответов уходит на api.fish.audio.").color(DIM).size(11.5));
            changed |= field(ui, "Ключ Fish Audio", &mut self.cfg.fish_key, "пусто — встроенный голос", true);
            changed |= field(ui, "Номер голоса", &mut self.cfg.fish_voice, "идентификатор голоса", false);
            ui.add_space(8.0);

            ui.label(RichText::new("Слово «Джарвис»").font(semi(13.5)).color(ACCENT));
            changed |= ui.add_enabled(self.ears.ready(), egui::Checkbox::new(&mut self.cfg.jarvis_listen, "Слушать постоянно и откликаться на имя")).changed();
            ui.label(RichText::new("Микрофон включён всё время. Речь распознаётся на этом компьютере; фразы без слова «Джарвис» сразу отбрасываются и нигде не сохраняются.").color(DIM).size(11.5));
        });
        if changed {
            self.cfg.save();
        }
        if let Some(id) = download {
            self.jv_download(id, &ctx);
        }
    }

    pub(crate) fn ui_jarvis(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.jv_view == 1 {
            self.ui_jarvis_settings(ui);
            return;
        }
        // думать нечем: ни локальной модели, ни облачного мозга — предлагаем установку
        let prog = self.setup.progress();
        if prog.running || (!self.setup.brain_ready() && self.jv_remote().is_none()) {
            self.ui_jarvis_setup(ui, &prog);
            return;
        }
        let cur = self.jv_model();
        let loaded = self.jarvis.loaded();
        let (mut pick, mut clear) = (None, false);
        ui.horizontal(|ui| {
            for (id, name, _) in MODELS {
                let have = self.jarvis.model_path(id).exists();
                let label = if id == cur { format!("●  {name}") } else { name.to_string() };
                if ui.add_enabled(have && !self.jv_busy, egui::Button::new(label)).on_disabled_hover_text(if have { "Подождите ответа" } else { "Модель ещё не скачана" }).clicked() {
                    pick = Some(id);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if jelly(ui, "Настройки").clicked() {
                    self.jv_view = 1;
                }
                clear = jelly(ui, "Очистить").clicked();
            });
        });
        let heard = if self.jv_heard > 0.0 { format!(" · распознавание {:.1} с ({})", self.jv_heard, self.jv_engine) } else { String::new() };
        let cloud = self.jv_remote().is_some();
        let state = match (&loaded, self.jv_stats.is_empty()) {
            (Some(_), false) => format!("Модель в памяти · {}{}", self.jv_stats, heard),
            (Some(_), true) => "Модель в памяти".to_string(),
            (None, false) => format!("{} · {}{}", if cloud { "Облачный мозг" } else { "Модель выгружена" }, self.jv_stats, heard),
            (None, true) if cloud => "Облачный мозг: память компьютера не занята".to_string(),
            (None, true) => "Модель выгружена — загрузится при первом вопросе".to_string(),
        };
        let mut flip_listen = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new(state).color(DIM).size(11.5));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                flip_listen = ui
                    .add_enabled(self.ears.ready(), egui::Button::new(if self.cfg.jarvis_listen { "Слово «Джарвис»: слушаю" } else { "Слово «Джарвис»: выкл" }))
                    .on_hover_text("Микрофон включён постоянно. Речь распознаётся на этом компьютере и никуда не отправляется; остров реагирует только на фразы со словом «Джарвис»")
                    .clicked();
            });
        });
        if flip_listen {
            self.cfg.jarvis_listen = !self.cfg.jarvis_listen;
            self.cfg.save();
        }
        if let Some(id) = pick {
            self.cfg.jarvis_model = id.to_string();
            self.cfg.save();
        }
        if clear && !self.jv_busy {
            self.jv_log.clear();
            self.jv_msgs.clear();
            self.jv_stats.clear();
        }
        ui.add_space(2.0);

        let extra = if self.jv_confirm.is_some() { 64.0 } else { 0.0 };
        let list_h = (ui.available_height() - 40.0 - extra).max(60.0);
        egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            if self.jv_log.is_empty() && !self.jv_busy {
                empty_state(ui, "\u{E99A}", "Джарвис готов", "Напишите или нажмите «Говорить» и скажите своими словами: открой загрузки, сделай тише");
            }
            for (kind, text) in &self.jv_log {
                match kind {
                    0 => {
                        ui.add_space(4.0);
                        ui.label(RichText::new(text).color(ACCENT));
                    }
                    1 => {
                        ui.label(RichText::new(text).color(FG));
                    }
                    2 => {
                        ui.label(RichText::new(format!("→  {text}")).color(Color32::from_rgb(0x7B, 0xE0, 0x9A)).size(12.0));
                    }
                    _ => {
                        ui.label(RichText::new(text).color(Color32::from_rgb(0xFF, 0x8A, 0x80)));
                    }
                }
            }
            if self.jv_busy {
                ui.label(RichText::new(&self.jv_status).color(DIM));
            }
            match self.jv_listen {
                1 => {
                    // полоска громкости: видно, что микрофон вас слышит
                    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
                    let p = ui.painter();
                    p.text(r.left_center(), Align2::LEFT_CENTER, "Слушаю…", FontId::proportional(13.0), DIM);
                    let bar = egui::Rect::from_min_size(r.left_center() + vec2(76.0, -3.0), vec2(220.0, 6.0));
                    p.rect_filled(bar, 3.0, Color32::from_white_alpha(22));
                    p.rect_filled(egui::Rect::from_min_size(bar.min, vec2(bar.width() * self.jv_level.clamp(0.02, 1.0), 6.0)), 3.0, ACCENT);
                }
                2 => {
                    ui.label(RichText::new("Разбираю слова…").color(DIM));
                }
                _ => {}
            }
        });

        // необратимое действие ждёт разрешения
        let mut answer = None;
        if let Some((text, _)) = &self.jv_confirm {
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::hover());
            glass_chip(ui.painter(), row, 12.0, 0.7);
            let p = ui.painter().with_clip_rect(row.shrink(6.0));
            let mut lines = text.lines();
            p.text(row.left_top() + vec2(14.0, 9.0), Align2::LEFT_TOP, lines.next().unwrap_or(""), semi(13.5), FG);
            p.text(row.left_top() + vec2(14.0, 31.0), Align2::LEFT_TOP, lines.collect::<Vec<_>>().join(" "), FontId::monospace(11.5), DIM);
            let mut btns = ui.new_child(egui::UiBuilder::new().max_rect(row.shrink2(vec2(10.0, 13.0))).layout(egui::Layout::right_to_left(egui::Align::Center)));
            if jelly(&mut btns, "Нет").clicked() {
                answer = Some(false);
            }
            if jelly(&mut btns, "Да").clicked() {
                answer = Some(true);
            }
        }
        if let Some(yes) = answer {
            if let Some((_, reply)) = self.jv_confirm.take() {
                let _ = reply.send(yes);
            }
        }

        let (mut send, mut mic) = (false, false);
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 216.0).max(120.0);
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.jv_input)
                    .margin(egui::Margin::symmetric(9.0, 7.0))
                    .hint_text(RichText::new("Скажите Джарвису, что сделать").color(DIM))
                    .desired_width(w),
            );
            send = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            send |= ui.add_enabled(!self.jv_busy, egui::Button::new("Отправить")).clicked();
            let label = match self.jv_listen {
                0 => "Говорить",
                1 => "Готово",
                _ => "…",
            };
            mic = ui.add_enabled(self.jv_listen != 2 && (!self.jv_busy || self.jv_listen == 1), egui::Button::new(label)).on_hover_text("Одна фраза с микрофона. Запись сама закончится на паузе").clicked();
            if send {
                r.request_focus();
            }
        });
        if mic {
            self.jarvis_listen(&ctx);
        }
        if send {
            let q = std::mem::take(&mut self.jv_input);
            self.jarvis_ask(q, &ctx);
        }
    }
}
