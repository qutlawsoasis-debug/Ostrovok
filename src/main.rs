#![windows_subsystem = "windows"]
//! Островок — живая панель в верхней части экрана Windows.

mod ai;
mod fx;
mod convert;
mod ears;
mod glass;
mod install;
mod jarvis;
mod jarvis_ui;
mod setup;
mod sound;
mod live;
mod media;
mod store;
mod update;
mod sys;
mod voice;
mod weather;

use eframe::egui::{self, pos2, vec2, Align2, Color32, FontId, Id, Pos2, Rect, RichText, Rounding, Sense, Stroke, Vec2};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const WIN_W: f32 = 780.0;
const AI_H: f32 = 620.0;
/// Способ размытия фона по умолчанию: acrylic, blur или off.
const DEFAULT_BLUR: &str = "off";
const TOP: f32 = 6.0;
const PILL_H: f32 = 36.0;
const PANEL_W: f32 = 700.0;
const ROW_H: f32 = 34.0;

const FG: Color32 = Color32::from_rgb(250, 250, 252);
const DIM: Color32 = Color32::from_rgb(196, 200, 212);
/// Тема оформления: название, цвет акцента и оттенок тёмного стекла.
struct Theme {
    name: &'static str,
    accent: [u8; 3],
    glass: [u8; 3],
}

const THEMES: [Theme; 6] = [
    Theme { name: "Лаванда", accent: [185, 164, 255], glass: [11, 11, 20] },
    Theme { name: "Океан", accent: [112, 198, 255], glass: [7, 13, 24] },
    Theme { name: "Мята", accent: [118, 232, 182], glass: [7, 17, 15] },
    Theme { name: "Закат", accent: [255, 158, 112], glass: [22, 11, 10] },
    Theme { name: "Сакура", accent: [255, 152, 204], glass: [21, 9, 17] },
    Theme { name: "Графит", accent: [226, 229, 236], glass: [13, 13, 14] },
];

/// Номер выбранной темы. Цвета читают отовсюду, поэтому он общий.
static THEME: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn theme() -> &'static Theme {
    &THEMES[THEME.load(Ordering::Relaxed).min(THEMES.len() - 1)]
}

/// Цвет акцента выбранной темы.
fn accent() -> Color32 {
    let a = theme().accent;
    Color32::from_rgb(a[0], a[1], a[2])
}
const BG: Color32 = Color32::from_rgb(11, 11, 20);

/// Цвет с обычной прозрачностью (без пересчёта в линейное пространство).
fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color32 {
    let m = |c: u8| (c as u16 * a as u16 / 255) as u8;
    Color32::from_rgba_premultiplied(m(r), m(g), m(b), a)
}

/// Полупрозрачный белый для кнопок и карточек на стекле.
fn tint(a: u8) -> Color32 {
    rgba(255, 255, 255, a / 2 + 6)
}

/// Контур скруглённого прямоугольника, сдвинутый внутрь на inset: точки и внешние нормали.
fn contour(rect: Rect, r: f32, inset: f32, n: usize) -> Vec<(Pos2, Vec2)> {
    let core = rect.shrink(r);
    let ri = (r - inset).max(0.0);
    let corners = [(core.left_top(), 180.0f32), (core.right_top(), 270.0), (core.right_bottom(), 0.0), (core.left_bottom(), 90.0)];
    let mut out = Vec::with_capacity(4 * (n + 1));
    for (c, start) in corners {
        for i in 0..=n {
            let a = (start + 90.0 * i as f32 / n as f32).to_radians();
            let dir = vec2(a.cos(), a.sin());
            out.push((c + dir * ri, dir));
        }
    }
    out
}

/// Тестовый запуск (OSTROVOK_TEST): окно за пределами экрана, не поверх остальных, без горячей клавиши.
/// Нужен, чтобы проверять вёрстку в фоне и не мешать тому, что открыто на экране.
fn test_mode() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("OSTROVOK_TEST").is_some())
}

/// Пружина: значение догоняет цель и немного колеблется. Параметры и шаг — как в Atoll (Services/Spring.cs, MIT).
#[derive(Clone, Copy)]
struct Spr {
    v: f32,
    vel: f32,
    target: f32,
    k: f32,
    d: f32,
    eps: f32,
}

impl Spr {
    const fn new(v: f32, k: f32, d: f32, eps: f32) -> Self {
        Self { v, vel: 0.0, target: v, k, d, eps }
    }

    fn settled(&self) -> bool {
        (self.target - self.v).abs() < self.eps && self.vel.abs() < self.eps * 3.0
    }

    fn step(&mut self, dt: f32) {
        let n = (dt / 0.004).ceil().max(1.0);
        let h = dt / n;
        for _ in 0..n as usize {
            self.vel += (self.k * (self.target - self.v) - self.d * self.vel) * h;
            self.v += self.vel * h;
        }
        if self.settled() {
            self.v = self.target;
            self.vel = 0.0;
        }
    }
}

/// Живое уведомление: на несколько секунд пилюля становится карточкой со значком, заголовком и текстом.
/// Только что сделанный снимок экрана: его показывают карточкой с действиями.
struct ShotCard {
    path: String,
    tex: egui::TextureHandle,
    size: (usize, usize),
    /// сам снимок — для распознавания текста
    rgba: Arc<Vec<u8>>,
    /// когда карточка сама свернётся, если её не тронули
    until: Instant,
}

/// Картинка из истории буфера: полный размер — чтобы вернуть в буфер, миниатюра — для показа.
struct ClipImg {
    w: usize,
    h: usize,
    px: Vec<u8>,
    hash: u64,
    tex: egui::TextureHandle,
}

/// Быстрый отпечаток картинки: FNV по каждой 97-й точке и размеру — чтобы не хранить одинаковые снимки дважды.
fn img_hash(px: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64 ^ px.len() as u64;
    for b in px.iter().step_by(97) {
        h = (h ^ *b as u64).wrapping_mul(0x100000001b3);
    }
    h
}

/// Размер миниатюры в полосе: высота фиксирована, соотношение сторон зажато между 3:4 и 16:9.
fn thumb_size(w: usize, h: usize, height: f32) -> Vec2 {
    if w == 0 || h == 0 {
        return vec2(0.0, 0.0);
    }
    vec2(height * (w as f32 / h as f32).clamp(0.75, 16.0 / 9.0), height)
}

#[cfg(test)]
mod clip_img_tests {
    use super::*;
    #[test]
    fn thumbs_and_hashes() {
        assert_eq!(thumb_size(1920, 1080, 54.0), vec2(96.0, 54.0));
        assert_eq!(thumb_size(100, 1000, 54.0), vec2(40.5, 54.0));
        assert_eq!(thumb_size(0, 10, 54.0), vec2(0.0, 0.0));
        let a = vec![7u8; 4000];
        let mut b = a.clone();
        assert_eq!(img_hash(&a), img_hash(&a.clone()));
        b[97 * 3] = 9;
        assert_ne!(img_hash(&a), img_hash(&b));
        assert_ne!(img_hash(&a), img_hash(&a[..3996]));
    }
}

struct InboxItem {
    app: String,
    title: String,
    body: String,
    color: [u8; 3],
    at: chrono::DateTime<chrono::Local>,
}

/// Мессенджеры во входящих: имя, сколько сообщений, цвет — в порядке первого (самого свежего) сообщения.
fn inbox_apps(list: &[InboxItem]) -> Vec<(String, usize, [u8; 3])> {
    let mut by_app: Vec<(String, usize, [u8; 3])> = Vec::new();
    for it in list {
        match by_app.iter_mut().find(|a| a.0 == it.app) {
            Some(a) => a.1 += 1,
            None => by_app.push((it.app.clone(), 1, it.color)),
        }
    }
    by_app
}

/// Номера сообщений, которые видны при фильтре по мессенджеру (None — все).
fn inbox_visible(list: &[InboxItem], app: Option<&str>) -> Vec<usize> {
    (0..list.len()).filter(|&i| app.is_none_or(|a| list[i].app == a)).collect()
}

fn badge_text(n: usize) -> String {
    if n > 99 { "99+".into() } else { n.to_string() }
}

fn badge_w(ctx: &egui::Context, n: usize) -> f32 {
    (ctx.fonts(|f| f.layout_no_wrap(badge_text(n), semi(11.5), FG).size().x) + 12.0).max(18.0)
}

/// «5 сообщений», «1 сообщение», «22 сообщения».
fn count_ru(n: usize, forms: [&str; 3]) -> String {
    let form = match (n % 10, n % 100) {
        (_, 11..=14) => forms[2],
        (1, _) => forms[0],
        (2..=4, _) => forms[1],
        _ => forms[2],
    };
    format!("{n} {form}")
}

/// Сколько прошло: «только что», «14 мин», «2 ч», дальше — время по часам.
fn ago_ru(mins: i64, clock: &str) -> String {
    match mins {
        i64::MIN..=0 => "только что".into(),
        1..=59 => format!("{mins} мин"),
        60..=359 => format!("{} ч", mins / 60),
        _ => clock.to_string(),
    }
}

#[cfg(test)]
mod inbox_tests {
    use super::*;
    #[test]
    fn russian_counts() {
        let f = ["сообщение", "сообщения", "сообщений"];
        assert_eq!(count_ru(1, f), "1 сообщение");
        assert_eq!(count_ru(3, f), "3 сообщения");
        assert_eq!(count_ru(5, f), "5 сообщений");
        assert_eq!(count_ru(11, f), "11 сообщений");
        assert_eq!(count_ru(21, f), "21 сообщение");
        assert_eq!(count_ru(112, f), "112 сообщений");
        assert_eq!(ago_ru(0, "12:00"), "только что");
        assert_eq!(ago_ru(14, "12:00"), "14 мин");
        assert_eq!(ago_ru(95, "12:00"), "1 ч");
        assert_eq!(ago_ru(400, "05:20"), "05:20");
    }
}

struct Toast {
    glyph: &'static str,
    color: [u8; 3],
    title: String,
    text: String,
    until: Instant,
    /// показать обложку трека вместо значка
    cover: bool,
    /// шкала вместо текста (громкость), 0…1
    level: Option<f32>,
    /// маленькая всплывашка в одну строку: громкость, раскладка
    compact: bool,
}

/// Как контур острова отклоняется от ровной капсулы в этом кадре.
#[derive(Clone, Copy, Default)]
struct Warp {
    /// 0…1 — насколько тело тянется к курсору
    reach: f32,
    cursor: Pos2,
    /// высота волны, бегущей по краю, и её фаза
    ripple: f32,
    phase: f32,
    /// «капли» по нижней кромке под музыку
    drip: f32,
    drip_phase: f32,
}

impl Warp {
    /// pos и n — точка ровного контура и её нормаль, s — доля пути вдоль контура (0…1).
    fn apply(&self, rect: Rect, pos: Pos2, n: Vec2, s: f32) -> Pos2 {
        let (w, h) = (rect.width().max(1.0), rect.height().max(1.0));
        let yrel = ((pos.y - rect.top()) / h).clamp(0.0, 1.0);
        let mut out = pos;
        if self.reach > 0.01 {
            // всё тело течёт к курсору: сильнее всего та сторона, что к нему обращена; верх держится на месте
            let to = self.cursor - rect.center();
            let g = if to.length() > 1.0 { to.normalized() } else { vec2(0.0, 1.0) };
            let dist = (self.cursor - pos).length();
            let facing = n.dot(g).max(0.0);
            let facing = facing * facing * (3.0 - 2.0 * facing);
            let spread = 60.0 + w * 0.45;
            let near = 1.0 / (1.0 + (dist / spread) * (dist / spread));
            out += g * (18.0 * self.reach * (0.12 + 0.88 * facing) * near * yrel.powf(0.8));
        }
        if self.ripple > 0.03 {
            let waves = ((2.0 * (w + h)) / 150.0).round().max(3.0);
            out += n * (self.ripple * (0.3 + 0.7 * yrel) * (std::f32::consts::TAU * waves * s - self.phase).sin());
        }
        if self.drip > 0.0 && n.y > 0.7 {
            let t = ((pos.x - rect.left()) / w).clamp(0.0, 1.0);
            let env = (std::f32::consts::PI * t).sin();
            out.y += (self.drip * env * (0.55 + 0.45 * (t * 9.0 + self.drip_phase).sin() * (t * 4.0 - self.drip_phase * 0.7).cos())).max(0.0);
        }
        out
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Pill,
    Panel,
    Palette,
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Home,
    Music,
    Control,
    Remind,
    Qr,
    Shelf,
    Ai,
    Jarvis,
    Weather,
    Timer,
    Clip,
    Inbox,
    Notes,
    Todo,
    /// карточка только что сделанного снимка экрана (в меню разделов её нет)
    Shot,
    Calendar,
    System,
    Sound,
    Settings,
}

#[derive(PartialEq, Clone, Copy)]
enum Emotion {
    Normal,
    Happy,
    Love,
    Angry,
    Surprised,
    Cool,
    Sleepy,
}

enum Msg {
    Note(String),
    City(u64, u64, f64, f64, String, String),
    /// неудача поиска города: (номер запроса, текст)
    CityFailed(u64, String),
    Weather(u64, weather::Now),
    WeatherFailed(u64, String),
    Ai(Result<String, String>),
    Jarvis(jarvis::Ev),
    Heard(ears::Heard),
    /// снимок области пришёл из буфера обмена: ширина, высота, точки
    Shot(usize, usize, Vec<u8>),
    Download(String),
    Notice(live::Notice),
    Lyrics(String, Vec<(f64, String)>),
    /// значок программы для строки поиска, загруженный в фоне: путь и точки
    Icon(String, Option<(usize, usize, Vec<u8>)>),
    /// картинка из буфера обмена, прочитанная в фоне
    ClipImg(usize, usize, Vec<u8>),
}

#[derive(Clone)]
enum Action {
    Launch(String),
    Url(String),
    Copy(String),
    Timer(f64),
    Ask(String),
    Quick(Quick),
}

/// Запоминать ли запрос в «Недавнем»: программы, сайты, ответы и таймеры. Быстрые команды
/// («заметка …», «дело …», «напомни …») и вопросы к ИИ несут личный текст — их не храним.
fn recent_worthy(a: &Action) -> bool {
    matches!(a, Action::Launch(_) | Action::Url(_) | Action::Copy(_) | Action::Timer(_))
}

/// Кладёт запрос в начало «Недавнего»: без повторов (без учёта регистра), не больше пяти, до 60 символов.
fn push_recent(list: &mut Vec<String>, q: &str) {
    let q = q.trim();
    if q.is_empty() || q.chars().count() > 60 {
        return;
    }
    let low = q.to_lowercase();
    list.retain(|r| r.to_lowercase() != low);
    list.insert(0, q.to_string());
    list.truncate(5);
}

/// Быстрые команды строки поиска: «дело …», «заметка …», «напомни 18:30 …».
#[derive(Clone, Debug, PartialEq)]
enum Quick {
    Todo(String),
    Note(String),
    Remind(String, chrono::NaiveDateTime),
}

/// Разбирает быструю команду. `now` — местное время (передаётся снаружи, чтобы проверять тестами).
fn parse_quick(q: &str, now: chrono::NaiveDateTime) -> Option<Quick> {
    let q = q.trim();
    let low = q.to_lowercase();
    let after = |prefixes: &[&str]| prefixes.iter().find_map(|p| low.strip_prefix(p).and_then(|_| q.get(p.len()..)).map(|r| r.trim().to_string()));
    if let Some(t) = after(&["дело ", "todo "]).filter(|t| !t.is_empty()) {
        return Some(Quick::Todo(t));
    }
    if let Some(t) = after(&["заметка ", "note "]).filter(|t| !t.is_empty()) {
        return Some(Quick::Note(t));
    }
    let rest = after(&["напомни мне ", "напомнить ", "напомни "])?;
    let words: Vec<&str> = rest.split_whitespace().collect();
    let clock = |w: &str| chrono::NaiveTime::parse_from_str(&w.replace('.', ":"), "%H:%M").ok();
    let at_clock = |t: chrono::NaiveTime| {
        let today = now.date().and_time(t);
        if today <= now { today + chrono::Duration::days(1) } else { today }
    };
    let unit = |w: &str| -> Option<i64> {
        let w = w.to_lowercase();
        if w.starts_with("мин") || w == "м" {
            Some(1)
        } else if w.starts_with('ч') {
            Some(60)
        } else {
            None
        }
    };
    let text_of = |skip: std::ops::Range<usize>| words.iter().enumerate().filter(|(i, _)| !skip.contains(i)).map(|(_, w)| *w).collect::<Vec<_>>().join(" ");
    for i in 0..words.len() {
        let w = words[i].to_lowercase();
        // «в 18:30» или «18:30»
        let (start, t) = if w == "в" && i + 1 < words.len() { (i, clock(words[i + 1]).map(|t| (t, i + 2))) } else { (i, clock(&w).map(|t| (t, i + 1))) };
        if let Some((t, end)) = t {
            let text = text_of(start..end);
            return (!text.is_empty()).then(|| Quick::Remind(text, at_clock(t)));
        }
        // «через 15 мин», «через час», «через 2 часа»
        if w == "через" && i + 1 < words.len() {
            let (n, unit_at) = match words[i + 1].replace(',', ".").parse::<f64>() {
                Ok(n) => (n, i + 2),
                Err(_) => (1.0, i + 1),
            };
            if let Some(mul) = words.get(unit_at).and_then(|u| unit(u)) {
                let text = text_of(i..unit_at + 1);
                // nan/inf/огромные числа: не дальше года, иначе Duration и сложение дат падают
                let mins = n * mul as f64;
                if !mins.is_finite() || mins.round() < 1.0 || mins > 366.0 * 24.0 * 60.0 {
                    return None;
                }
                let at = now.checked_add_signed(chrono::Duration::minutes(mins.round() as i64))?;
                return (!text.is_empty()).then(|| Quick::Remind(text, at));
            }
        }
    }
    None
}

/// Правило летнего времени.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Dst {
    No,
    /// ЕС: с последнего воскресенья марта по последнее воскресенье октября, переход в 01:00 UTC
    Eu,
    /// США и Канада: со второго воскресенья марта по первое воскресенье ноября, в 02:00 местного
    Us,
    /// юго-восток Австралии: с первого воскресенья октября (02:00) по первое воскресенье апреля (03:00 летнего)
    Au,
}

/// Город для мировых часов: как его можно назвать, как показывать, смещение от UTC зимой в минутах.
struct City {
    names: &'static [&'static str],
    show: &'static str,
    offset: i32,
    dst: Dst,
}

const CITIES: &[City] = &[
    City { names: &["москва", "moscow", "мск"], show: "Москва", offset: 180, dst: Dst::No },
    City { names: &["санкт-петербург", "петербург", "питер", "spb", "saint petersburg"], show: "Петербург", offset: 180, dst: Dst::No },
    City { names: &["калининград", "kaliningrad"], show: "Калининград", offset: 120, dst: Dst::No },
    City { names: &["екатеринбург", "yekaterinburg"], show: "Екатеринбург", offset: 300, dst: Dst::No },
    City { names: &["новосибирск", "novosibirsk"], show: "Новосибирск", offset: 420, dst: Dst::No },
    City { names: &["владивосток", "vladivostok"], show: "Владивосток", offset: 600, dst: Dst::No },
    City { names: &["киев", "київ", "kyiv", "kiev"], show: "Киев", offset: 120, dst: Dst::Eu },
    City { names: &["минск", "minsk"], show: "Минск", offset: 180, dst: Dst::No },
    City { names: &["алматы", "almaty"], show: "Алматы", offset: 300, dst: Dst::No },
    City { names: &["ташкент", "tashkent"], show: "Ташкент", offset: 300, dst: Dst::No },
    City { names: &["тбилиси", "tbilisi"], show: "Тбилиси", offset: 240, dst: Dst::No },
    City { names: &["стамбул", "istanbul"], show: "Стамбул", offset: 180, dst: Dst::No },
    City { names: &["дубай", "dubai"], show: "Дубай", offset: 240, dst: Dst::No },
    City { names: &["лондон", "london"], show: "Лондон", offset: 0, dst: Dst::Eu },
    City { names: &["берлин", "berlin", "германия"], show: "Берлин", offset: 60, dst: Dst::Eu },
    City { names: &["париж", "paris"], show: "Париж", offset: 60, dst: Dst::Eu },
    City { names: &["варшава", "warsaw"], show: "Варшава", offset: 60, dst: Dst::Eu },
    City { names: &["дели", "delhi", "индия", "мумбаи"], show: "Дели", offset: 330, dst: Dst::No },
    City { names: &["пекин", "beijing", "шанхай", "китай"], show: "Пекин", offset: 480, dst: Dst::No },
    City { names: &["токио", "tokyo", "япония"], show: "Токио", offset: 540, dst: Dst::No },
    City { names: &["сеул", "seoul"], show: "Сеул", offset: 540, dst: Dst::No },
    City { names: &["сидней", "sydney", "мельбурн", "melbourne"], show: "Сидней", offset: 600, dst: Dst::Au },
    City { names: &["нью-йорк", "нью йорк", "new york", "nyc"], show: "Нью-Йорк", offset: -300, dst: Dst::Us },
    City { names: &["чикаго", "chicago"], show: "Чикаго", offset: -360, dst: Dst::Us },
    City { names: &["лос-анджелес", "лос анджелес", "los angeles", "la"], show: "Лос-Анджелес", offset: -480, dst: Dst::Us },
    City { names: &["торонто", "toronto"], show: "Торонто", offset: -300, dst: Dst::Us },
    City { names: &["utc", "гринвич", "gmt"], show: "UTC", offset: 0, dst: Dst::No },
];

const MONTHS_GEN: [&str; 12] = ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];
const WEEKDAYS: [&str; 7] = ["понедельник", "вторник", "среда", "четверг", "пятница", "суббота", "воскресенье"];

/// «сегодня», «завтра», «через 44 дня», «3 дня назад».
fn days_phrase(n: i64) -> String {
    match n {
        0 => "сегодня".into(),
        1 => "завтра".into(),
        -1 => "вчера".into(),
        n if n > 0 => format!("через {}", count_ru(n as usize, ["день", "дня", "дней"])),
        n => format!("{} назад", count_ru((-n) as usize, ["день", "дня", "дней"])),
    }
}

/// Счёт дней в поиске: «через 45 дней», «через 2 месяца», «до нового года», «до 8 марта», «15.02.2027».
/// Возвращает дату и строку ответа.
fn date_query(q: &str, today: chrono::NaiveDate) -> Option<(chrono::NaiveDate, String)> {
    use chrono::Datelike;
    let q = q.trim().to_lowercase();
    let pretty = |d: chrono::NaiveDate| format!("{} {} {}", d.day(), MONTHS_GEN[d.month0() as usize], d.year());
    let wd = |d: chrono::NaiveDate| WEEKDAYS[d.weekday().num_days_from_monday() as usize];
    // дата без года: если уже прошла — в следующем году
    // ищем ближайший год, где такая дата есть и ещё не прошла (29 февраля — до 8 лет вперёд)
    let ahead = |d: u32, m: u32| -> Option<chrono::NaiveDate> {
        (today.year()..=today.year() + 8).filter_map(|y| chrono::NaiveDate::from_ymd_opt(y, m, d)).find(|x| *x >= today)
    };
    // «ДД.ММ» или «ДД.ММ.ГГГГ» — только если вся строка дата (чтобы не мешать калькулятору: «12.5»)
    let parse_dot = |s: &str| -> Option<chrono::NaiveDate> {
        let parts: Vec<&str> = s.split('.').collect();
        if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit())) {
            return None;
        }
        let (d, m) = (parts[0].parse::<u32>().ok()?, parts[1].parse::<u32>().ok()?);
        if parts[1].len() != 2 && parts.len() == 2 {
            return None; // «12.5» — это число, а не дата
        }
        match parts.get(2) {
            Some(y) => {
                let y: i32 = y.parse().ok()?;
                chrono::NaiveDate::from_ymd_opt(if y < 100 { 2000 + y } else { y }, m, d)
            }
            None => ahead(d, m),
        }
    };
    if let Some(rest) = q.strip_prefix("через ") {
        let words: Vec<&str> = rest.split_whitespace().collect();
        let (n, unit) = match words.as_slice() {
            [n, u] => (n.parse::<u32>().ok()?, *u),
            [u] => (1, *u),
            _ => return None,
        };
        let d = if unit.starts_with("дн") || unit.starts_with("ден") || unit.starts_with("сут") {
            today.checked_add_days(chrono::Days::new(n as u64))?
        } else if unit.starts_with("нед") {
            today.checked_add_days(chrono::Days::new(7 * n as u64))?
        } else if unit.starts_with("мес") {
            today.checked_add_months(chrono::Months::new(n))?
        } else if unit.starts_with("год") || unit.starts_with("лет") {
            today.checked_add_months(chrono::Months::new(n.checked_mul(12)?))?
        } else {
            return None; // «через 15 мин» — не наше
        };
        return Some((d, format!("{}, {}", pretty(d), wd(d))));
    }
    let (target, label) = if let Some(rest) = q.strip_prefix("до ") {
        let rest = rest.trim();
        let d = if rest.starts_with("нового года") || rest == "нг" {
            chrono::NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)?
        } else if let Some(d) = parse_dot(rest) {
            d
        } else {
            // «8 марта» или «8 марта 2027»
            let words: Vec<&str> = rest.split_whitespace().collect();
            let (day, month) = (words.first()?.parse::<u32>().ok()?, *words.get(1)?);
            let m = MONTHS_GEN.iter().position(|x| *x == month)? as u32 + 1;
            match words.get(2).and_then(|y| y.parse::<i32>().ok()) {
                Some(y) => chrono::NaiveDate::from_ymd_opt(if y < 100 { 2000 + y } else { y }, m, day)?,
                None => ahead(day, m)?,
            }
        };
        (d, true)
    } else {
        (parse_dot(&q)?, false)
    };
    let n = (target - today).num_days();
    let weeks = match (n >= 14, n % 7) {
        (true, 0) => format!(" ({} нед.)", n / 7),
        (true, r) => format!(" ({} нед. {r} дн.)", n / 7),
        _ => String::new(),
    };
    Some((target, if label { format!("{}{weeks} · {}, {}", days_phrase(n), pretty(target), wd(target)) } else { format!("{}, {} · {}", pretty(target), wd(target), days_phrase(n)) }))
}

#[cfg(test)]
mod date_tests {
    use super::*;
    fn d(y: i32, m: u32, day: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    #[test]
    fn date_queries() {
        let today = d(2026, 11, 17);
        assert_eq!(date_query("через 45 дней", today).unwrap().0, d(2027, 1, 1));
        assert_eq!(date_query("через 3 недели", today).unwrap().0, d(2026, 12, 8));
        assert_eq!(date_query("через месяц", d(2026, 1, 31)).unwrap().0, d(2026, 2, 28));
        assert_eq!(date_query("через 2 года", today).unwrap().0, d(2028, 11, 17));
        let (ny, t) = date_query("до нового года", today).unwrap();
        assert_eq!(ny, d(2027, 1, 1));
        assert!(t.starts_with("через 45 дней (6 нед. 3 дн.)"), "{t}");
        assert_eq!(date_query("до 31.12", today).unwrap().0, d(2026, 12, 31));
        assert_eq!(date_query("до 8 марта", today).unwrap().0, d(2027, 3, 8));
        assert!(date_query("до нового года", d(2026, 12, 31)).unwrap().1.starts_with("завтра"));
        assert_eq!(date_query("15.02.2027", today).unwrap().0, d(2027, 2, 15));
        assert!(date_query("15.11.2026", today).unwrap().1.ends_with("2 дня назад"));
        assert!(date_query("12.5", today).is_none());
        assert!(date_query("2*3", today).is_none());
        assert!(date_query("через 15 мин", today).is_none());
        assert!(date_query("31.02", today).is_none());
        assert_eq!(date_query("до 29 февраля", today).unwrap().0, d(2028, 2, 29));
        assert_eq!(date_query("до 8 марта 2027", today).unwrap().0, d(2027, 3, 8));
        assert!(date_query("через 400000000 лет", today).is_none());
        assert!(date_query("до 1 января", d(2026, 12, 4)).unwrap().1.contains("(4 нед.)"));
    }

    #[test]
    fn strange_input_does_not_panic() {
        let now = d(2026, 11, 17).and_hms_opt(12, 0, 0).unwrap();
        for q in ["", " ", "напомни ", "напомни через", "напомни через nan мин чай", "напомни через inf ч чай", "напомни через -5 мин чай",
            "напомни через 1e300 ч чай", "напомни через 99999999999999999999 мин чай", "дело 🙂", "İnote x", "заметка", "напомни в 25:99 чай"] {
            let _ = parse_quick(q, now);
        }
        assert!(parse_quick("напомни через nan мин чай", now).is_none());
        assert!(parse_quick("напомни через 1e300 ч чай", now).is_none());
        assert!(parse_quick("напомни через 15 мин чай", now).is_some());
        for q in ["", "до ", "через", "до 99999999.12", "до 0.0", "до 31 февраля 2028", "до 29 февраля 2100", "через 4294967295 недель", "🙂.🙂", "до 8 марта 99999999999"] {
            let _ = date_query(q, d(2026, 11, 17));
        }
        assert_eq!(parse_due("Поздравить до 29.02", d(2026, 11, 17)).1, Some(d(2028, 2, 29)));
        assert_eq!(parse_due("Сдать до 31.01", d(2026, 1, 31)).1, Some(d(2026, 1, 31)));
    }

    #[test]
    fn dst_night_times_resolve() {
        // в Германии 29.03.2026 02:30 нет, 25.10.2026 02:30 есть дважды — оба должны давать метку
        assert!(local_ts(d(2026, 3, 29).and_hms_opt(2, 30, 0).unwrap()).is_some());
        let a = local_ts(d(2026, 10, 25).and_hms_opt(2, 30, 0).unwrap()).unwrap();
        assert!(a < local_ts(d(2026, 10, 25).and_hms_opt(3, 30, 0).unwrap()).unwrap());
    }
}

/// Сколько городов помещается в ряд под календарём.
const WORLD_MAX: usize = 3;

/// Чистит список мировых часов: только известные города, в каноническом написании, без повторов, не больше WORLD_MAX.
fn sanitize_world(list: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in list.iter().filter_map(|n| find_city(n)) {
        if out.len() >= WORLD_MAX {
            break;
        }
        if !out.iter().any(|w| w == c.show) {
            out.push(c.show.to_string());
        }
    }
    out
}

fn find_city(q: &str) -> Option<&'static City> {
    let q = q.trim().to_lowercase().replace('ё', "е");
    if q.is_empty() {
        return None;
    }
    CITIES.iter().find(|c| c.names.iter().any(|n| *n == q) || c.show.to_lowercase() == q)
}

fn nth_sunday(y: i32, m: u32, n: u32) -> chrono::NaiveDate {
    use chrono::Datelike;
    let first = chrono::NaiveDate::from_ymd_opt(y, m, 1).unwrap();
    let shift = (7 - first.weekday().num_days_from_sunday()) % 7;
    first + chrono::Duration::days((shift + 7 * (n - 1)) as i64)
}

fn last_sunday(y: i32, m: u32) -> chrono::NaiveDate {
    use chrono::Datelike;
    let next = if m == 12 { chrono::NaiveDate::from_ymd_opt(y + 1, 1, 1) } else { chrono::NaiveDate::from_ymd_opt(y, m + 1, 1) }.unwrap();
    let last = next - chrono::Duration::days(1);
    last - chrono::Duration::days(last.weekday().num_days_from_sunday() as i64)
}

/// Смещение города от UTC в минутах в данный момент — с учётом летнего времени.
fn city_offset(c: &City, utc: chrono::NaiveDateTime) -> i32 {
    use chrono::Datelike;
    let y = utc.year();
    let summer = match c.dst {
        Dst::No => false,
        Dst::Eu => {
            let start = last_sunday(y, 3).and_hms_opt(1, 0, 0).unwrap();
            let end = last_sunday(y, 10).and_hms_opt(1, 0, 0).unwrap();
            utc >= start && utc < end
        }
        Dst::Us => {
            // 02:00 местного зимнего = 02:00 − смещение в UTC; конец — 02:00 местного летнего
            let start = nth_sunday(y, 3, 2).and_hms_opt(2, 0, 0).unwrap() - chrono::Duration::minutes(c.offset as i64);
            let end = nth_sunday(y, 11, 1).and_hms_opt(2, 0, 0).unwrap() - chrono::Duration::minutes(c.offset as i64 + 60);
            utc >= start && utc < end
        }
        Dst::Au => {
            // южное полушарие: лето захватывает Новый год
            let start = nth_sunday(y, 10, 1).and_hms_opt(2, 0, 0).unwrap() - chrono::Duration::minutes(c.offset as i64);
            let end = nth_sunday(y, 4, 1).and_hms_opt(3, 0, 0).unwrap() - chrono::Duration::minutes(c.offset as i64 + 60);
            utc >= start || utc < end
        }
    };
    c.offset + if summer { 60 } else { 0 }
}

/// Подпись разницы: «+3 ч», «−8 ч», «+2:30», «так же»; и «завтра» / «вчера», если там другой день.
fn world_label(here: chrono::NaiveDateTime, there: chrono::NaiveDateTime) -> String {
    // времена берутся из разных часов и расходятся на доли секунды — округляем до ближайшей минуты
    let diff = ((there - here).num_milliseconds() as f64 / 60_000.0).round() as i64;
    let sign = if diff < 0 { "−" } else { "+" };
    let a = diff.abs();
    let mut s = match (a / 60, a % 60) {
        (0, 0) => "так же".to_string(),
        (h, 0) => format!("{sign}{h} ч"),
        (h, m) => format!("{sign}{h}:{m:02}"),
    };
    match there.date().signed_duration_since(here.date()).num_days() {
        1 => s.push_str(" · завтра"),
        -1 => s.push_str(" · вчера"),
        _ => {}
    }
    s
}

#[cfg(test)]
mod world_tests {
    use super::*;
    #[test]
    fn sanitize_world_list() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(sanitize_world(&[]).is_empty());
        assert!(sanitize_world(&v(&["атлантида"])).is_empty());
        let ny = find_city("нью йорк").unwrap().show;
        let tk = find_city("токио").unwrap().show;
        let ld = find_city("лондон").unwrap().show;
        let br = find_city("berlin").unwrap().show;
        assert_eq!(sanitize_world(&v(&["атлантида", "нью йорк", "Токио"])), v(&[ny, tk]));
        assert_eq!(sanitize_world(&v(&["токио", "ТОКИО", tk])), v(&[tk]));
        assert_eq!(sanitize_world(&v(&["токио", "лондон", "x", "berlin", "нью йорк"])).len(), WORLD_MAX);
        assert_eq!(sanitize_world(&v(&["токио", "лондон", "berlin", "нью йорк"])), v(&[tk, ld, br]));
    }
    fn t(y: i32, m: u32, d: u32, h: u32, mi: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, mi, 0).unwrap()
    }
    #[test]
    fn world_clock() {
        assert_eq!(nth_sunday(2026, 3, 2), chrono::NaiveDate::from_ymd_opt(2026, 3, 8).unwrap());
        assert_eq!(last_sunday(2026, 10), chrono::NaiveDate::from_ymd_opt(2026, 10, 25).unwrap());
        let ny = find_city("Нью Йорк").unwrap();
        assert_eq!(city_offset(ny, t(2026, 3, 7, 12, 0)), -300);
        assert_eq!(city_offset(ny, t(2026, 3, 9, 12, 0)), -240);
        let berlin = find_city("BERLIN").unwrap();
        assert_eq!(city_offset(berlin, t(2026, 10, 25, 0, 30)), 120);
        assert_eq!(city_offset(berlin, t(2026, 10, 25, 1, 30)), 60);
        assert_eq!(city_offset(find_city("токио").unwrap(), t(2026, 7, 1, 0, 0)), 540);
        let syd = find_city("сидней").unwrap();
        assert_eq!(city_offset(syd, t(2026, 11, 17, 0, 0)), 660); // ноябрь — у них лето
        assert_eq!(city_offset(syd, t(2026, 7, 1, 0, 0)), 600); // июль — зима
        assert_eq!(city_offset(syd, t(2026, 1, 10, 0, 0)), 660);
        assert!(find_city("атлантида").is_none());
        let msk = t(2026, 11, 17, 23, 0);
        assert_eq!(world_label(msk, msk + chrono::Duration::minutes(150)), "+2:30 · завтра");
        assert_eq!(world_label(msk, msk - chrono::Duration::hours(8)), "−8 ч");
        assert_eq!(world_label(msk, msk), "так же");
        // расхождение в доли секунды не превращает «+8 ч» в «+7:59»
        assert_eq!(world_label(msk - chrono::Duration::hours(10), msk - chrono::Duration::hours(2) - chrono::Duration::milliseconds(300)), "+8 ч");
        assert_eq!(world_label(t(2026, 11, 17, 2, 0), t(2026, 11, 16, 18, 0)), "−8 ч · вчера");
    }
}

/// Прибавляет минуты фокуса к дню; лог отсортирован по дате и хранит 60 последних дней.
fn add_focus(log: &mut Vec<(String, u32)>, day: chrono::NaiveDate, min: u32) {
    let key = day.format("%Y-%m-%d").to_string();
    match log.iter_mut().find(|e| e.0 == key) {
        Some(e) => e.1 += min,
        None => log.push((key, min)),
    }
    log.sort_by(|a, b| a.0.cmp(&b.0));
    let extra = log.len().saturating_sub(60);
    log.drain(..extra);
}

fn focus_on(log: &[(String, u32)], day: chrono::NaiveDate) -> u32 {
    let key = day.format("%Y-%m-%d").to_string();
    log.iter().find(|e| e.0 == key).map_or(0, |e| e.1)
}

/// Минуты за последние 7 дней, сегодня — последним.
fn week_minutes(log: &[(String, u32)], today: chrono::NaiveDate) -> [u32; 7] {
    std::array::from_fn(|i| focus_on(log, today - chrono::Duration::days(6 - i as i64)))
}

/// Сколько дней подряд был фокус. Если сегодня ещё не занимались — считаем со вчера, чтобы утром серия не обнулялась.
fn focus_streak(log: &[(String, u32)], today: chrono::NaiveDate) -> u32 {
    let mut day = if focus_on(log, today) > 0 { today } else { today - chrono::Duration::days(1) };
    let mut n = 0;
    while focus_on(log, day) > 0 {
        n += 1;
        day -= chrono::Duration::days(1);
    }
    n
}

/// «45 мин», «1 ч», «1 ч 15 мин».
fn fmt_minutes(m: u32) -> String {
    match (m / 60, m % 60) {
        (0, r) => format!("{r} мин"),
        (h, 0) => format!("{h} ч"),
        (h, r) => format!("{h} ч {r} мин"),
    }
}

#[cfg(test)]
mod focus_tests {
    use super::*;
    fn d(y: i32, m: u32, day: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    #[test]
    fn focus_stats() {
        let mut log = Vec::new();
        assert_eq!(focus_streak(&log, d(2026, 1, 2)), 0);
        add_focus(&mut log, d(2025, 12, 31), 25);
        add_focus(&mut log, d(2026, 1, 1), 25);
        add_focus(&mut log, d(2026, 1, 1), 50);
        assert_eq!(focus_on(&log, d(2026, 1, 1)), 75);
        // через Новый год, сегодня ещё 0 — серия со вчера
        assert_eq!(focus_streak(&log, d(2026, 1, 2)), 2);
        add_focus(&mut log, d(2026, 1, 2), 10);
        assert_eq!(focus_streak(&log, d(2026, 1, 2)), 3);
        // разрыв
        assert_eq!(focus_streak(&log, d(2026, 1, 4)), 0);
        assert_eq!(week_minutes(&log, d(2026, 1, 2)), [0, 0, 0, 0, 25, 75, 10]);
        for k in 0..80 {
            add_focus(&mut log, d(2026, 3, 1) + chrono::Duration::days(k), 5);
        }
        assert_eq!(log.len(), 60);
        assert_eq!(fmt_minutes(45), "45 мин");
        assert_eq!(fmt_minutes(60), "1 ч");
        assert_eq!(fmt_minutes(75), "1 ч 15 мин");
    }
}

#[cfg(test)]
mod quick_tests {
    use super::*;
    fn at(h: u32, m: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 11, 17).unwrap().and_hms_opt(h, m, 0).unwrap()
    }
    #[test]
    fn quick_commands() {
        let now = at(15, 0);
        assert_eq!(parse_quick("дело купить хлеб", now), Some(Quick::Todo("купить хлеб".into())));
        assert_eq!(parse_quick("Заметка Идея: тёмная тема", now), Some(Quick::Note("Идея: тёмная тема".into())));
        assert_eq!(parse_quick("напомни 18:30 позвонить маме", now), Some(Quick::Remind("позвонить маме".into(), at(18, 30))));
        assert_eq!(parse_quick("напомни позвонить маме в 18:30", now), Some(Quick::Remind("позвонить маме".into(), at(18, 30))));
        assert_eq!(parse_quick("напомни через 15 мин чай", now), Some(Quick::Remind("чай".into(), at(15, 15))));
        assert_eq!(parse_quick("напомни через час выйти", now), Some(Quick::Remind("выйти".into(), at(16, 0))));
        assert_eq!(parse_quick("напомни выключить духовку через 2 часа", now), Some(Quick::Remind("выключить духовку".into(), at(17, 0))));
        // время уже прошло — завтра
        assert_eq!(parse_quick("напомни 9:00 зарядка", now), Some(Quick::Remind("зарядка".into(), at(9, 0) + chrono::Duration::days(1))));
        assert_eq!(parse_quick("напомни 18:30", now), None);
        assert_eq!(parse_quick("дело", now), None);
        assert_eq!(parse_quick("делать нечего", now), None);
    }
}

struct Item {
    title: String,
    sub: String,
    action: Action,
}

enum Seg {
    Thumb,
    Track(String),
    Eq,
    Text(String, Color32),
    /// непрочитанные сообщения: число на цветной капсуле мессенджера
    Badge(usize, Color32),
    /// заряд кончается: процент и идёт ли зарядка
    Battery(u8, bool),
}

struct App {
    mode: Mode,
    tab: Tab,
    size: Vec2,
    passthrough: Option<bool>,
    hidden: bool,
    outside_since: Option<Instant>,
    last_slow: Instant,

    emotion: Emotion,
    emotion_until: Instant,
    next_blink: Instant,
    blink_until: Instant,

    note: Option<(String, Instant)>,
    timer_end: Option<Instant>,
    timer_custom: String,

    media: Arc<Mutex<media::State>>,
    media_tx: mpsc::Sender<media::Cmd>,
    media_now: Option<media::Info>,
    thumb: Option<egui::TextureHandle>,
    thumb_gen: u64,

    weather: Option<weather::Now>,
    weather_at: Option<Instant>,
    weather_err: Option<String>,
    weather_fails: u32,
    /// поколение запросов погоды: ответы для прежнего города отбрасываются
    weather_gen: u64,
    /// номер последнего запроса города: принимается только ответ на него
    city_req: u64,
    cfg: store::Settings,
    city_edit: String,
    autostart: bool,

    notes: Vec<String>,
    /// какая заметка открыта и что ищут в списке
    note_sel: usize,
    note_query: String,
    notes_dirty: Option<Instant>,

    clipboard: Option<arboard::Clipboard>,
    clips: Vec<String>,
    clip_query: String,
    /// курсы валют для конвертера в строке поиска
    rates: convert::Rates,
    rates_shown: bool,
    palette_demo: bool,
    shot_card: Option<ShotCard>,
    /// рамки разметки на снимке (в его координатах) и начало протягивания по миниатюре
    shot_marks: Vec<[u32; 4]>,
    shot_drag: Option<Pos2>,
    clip_seq: u32,

    hotkey: Arc<AtomicBool>,
    hotkey_name: &'static str,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,

    apps: Arc<Mutex<Vec<(String, String)>>>,
    query: String,
    results: Vec<Item>,
    sel: usize,
    focus_query: bool,
    palette_at: Instant,

    shot: Option<u8>,

    vel: Vec2,
    sticky: bool,
    /// корпус на видеокарте; None — видеокарта не справилась, рисуем по-старому
    fx: Option<Arc<fx::Fx>>,
    // капли, отпочковавшиеся от пилюли: таймер и звонок
    bub_timer: Spr,
    bub_call: Spr,
    bub_clip: Spr,
    bub_dl: Spr,
    /// последняя подпись капли загрузки — чтобы она не пустела, пока капля втягивается
    dl_text: String,
    /// идущая загрузка: сколько байт скачано и скорость, байт в секунду
    dl: Arc<Mutex<Option<(u64, f32)>>>,
    hint: Option<Hint>,
    hint_until: Instant,
    clip_skip: bool,
    bubbles: Vec<(Rect, f32, u8, String)>,
    // остров-колба: высота и скорость поверхности жидкости по столбцам, уровень, место пилюли в окне
    liq_h: Vec<f32>,
    liq_v: Vec<f32>,
    level: f32,
    last_cx: f32,
    pill_rect: Rect,
    timer_total: f32,
    /// таймер на паузе: сколько оставалось (timer_end при этом пуст)
    timer_paused: Option<Duration>,
    // удобства Windows
    ctl_at: Instant,
    vol: Option<(f32, bool)>,
    mic: Option<bool>,
    bin: (i64, i64),
    light_theme: Option<bool>,
    mixer: Vec<sys::AppVol>,
    /// живой звук компьютера: источник и сглаженные столбики эквалайзера
    sound: sound::Sound,
    bars: [f32; sound::BANDS],
    beat: f32,
    /// какие программы сейчас звучат — для подписи, когда плеер молчит
    sounding: (String, Instant),
    mixer_at: Instant,
    /// громкость до «тихого режима» на странице «Звук»
    quiet_prev: Option<f32>,
    sheen_at: Instant,
    hover_was: bool,
    awake: bool,
    last_fg: isize,
    album: Option<[u8; 3]>,
    live: live::Live,
    /// последние уведомления мессенджеров, новые сверху; живут только в памяти
    inbox: Vec<InboxItem>,
    /// фильтр входящих по мессенджеру (None — все)
    inbox_filter: Option<String>,
    /// сколько пришло с тех пор, как открывали страницу «Уведомления»
    inbox_unread: usize,
    /// последние картинки из буфера обмена (только в памяти)
    clip_imgs: Vec<ClipImg>,
    /// строка ввода нового города для мировых часов (None — показываем кнопку)
    world_edit: Option<String>,
    world_focus: bool,
    /// город уже перепроверен на совпадение с часовым поясом
    city_rechecked: bool,
    /// запущен из %LOCALAPPDATA%\Programs\Ostrovok
    is_installed: bool,
    call_since: Option<Instant>,
    toast: Option<Toast>,
    last_track: String,
    app_icons: std::collections::HashMap<String, Option<egui::TextureHandle>>,
    // «жидкая» физика
    jelly: Spr,
    reach: Spr,
    lean_s: Spr,
    reach_cursor: Pos2,
    ripple: f32,
    ripple_phase: f32,
    drip_phase: f32,
    warp: Warp,
    glow: Option<(Color32, Instant)>,
    // жидкое стекло: снимок фона под островом
    lens_tx: std::sync::mpsc::Sender<glass::Req>,
    lens_rx: std::sync::mpsc::Receiver<glass::Frame>,
    lens_tex: Option<egui::TextureHandle>,
    /// область снимка в координатах поля и в координатах окна
    lens_cap: Option<Rect>,
    lens_draw: Option<Rect>,
    lens_key: [i32; 5],
    lens_at: Instant,
    lens_busy: bool,
    excluded: Option<bool>,
    capture_req: Option<bool>,
    app_query: Option<String>,
    confirm: Option<(&'static str, Instant)>,
    /// отложенная блокировка или сон: действие, когда, предупредили ли за минуту
    power_timer: Option<(&'static str, Instant, bool)>,
    /// что выбрано для отложенного действия: "lock" или "sleep"
    power_pick: &'static str,
    was_foreground: bool,
    qr_text: String,
    /// последний закодированный QR, чтобы не кодировать каждый кадр
    qr_cache: Option<QrCache>,
    /// палитра была открыта в прошлом кадре (чистка значков при закрытии)
    palette_was_open: bool,
    /// QR для Wi-Fi: режим, сеть, пароль (только в памяти, пока открыта страница), показать ли пароль, с паролем ли сеть
    qr_wifi: bool,
    wifi_ssid: String,
    wifi_pass: String,
    wifi_show: bool,
    wifi_secure: bool,
    pick_at: Option<Instant>,
    remind_text: String,
    remind_time: String,
    /// повтор для нового напоминания (см. repeat_name)
    remind_repeat: u8,
    lyrics: Vec<(f64, String)>,
    lyrics_key: String,
    /// системное размытие фона включено (None — ещё не пробовали)
    acrylic: Option<bool>,
    /// последнее заданное положение и размер настоящего окна
    win_last: Option<[f32; 4]>,

    // встроенный чат ИИ: веб-окно внутри панели
    web: Option<wry::WebView>,
    web_ctx: Option<Box<wry::WebContext>>,
    web_slot: Option<Rect>,
    web_rect: Option<Rect>,
    web_nav: Option<String>,
    web_visible: bool,
    web_error: Option<String>,
    tab_seen: Tab,
    tab_at: Instant,
    tab_dir: f32,
    mode_seen: Mode,
    mode_at: Instant,

    ai_chat: Vec<(bool, String)>,
    ai_input: String,
    ai_busy: bool,
    jarvis: jarvis::Jarvis,
    /// лента страницы: 0 — я, 1 — Джарвис, 2 — действие, 3 — ошибка
    jv_log: Vec<(u8, String)>,
    jv_msgs: Vec<serde_json::Value>,
    jv_input: String,
    jv_busy: bool,
    jv_status: String,
    jv_stats: String,
    jv_confirm: Option<(String, mpsc::Sender<bool>)>,
    ears: ears::Ears,
    setup: setup::Setup,
    /// страница Джарвиса: 0 — разговор, 1 — настройки
    jv_view: u8,
    /// какую модель ставить при первой установке
    jv_pick: &'static str,
    voice: voice::Voice,
    /// 0 — микрофон выключен, 1 — слушает, 2 — распознаёт
    jv_listen: u8,
    jv_level: f32,
    /// сколько секунд заняло последнее распознавание речи
    jv_heard: f32,
    jv_engine: &'static str,
    /// до этого момента следующая фраза считается просьбой и без имени (после «Джарвис» и паузы)
    jv_attn: Option<Instant>,
    key_edit: String,

    sw_start: Option<Instant>,
    sw_acc: Duration,
    cal_shift: i32,
    /// выбранный в календаре день (None — сегодня) и поля нового дела на него
    cal_sel: Option<chrono::NaiveDate>,
    cal_text: String,
    cal_time: String,
    meter: sys::CpuMeter,
    stats: sys::Stats,
    stats_at: Instant,
    /// нагрузка за последние минуты: доля процессора и памяти на каждый замер
    load_hist: std::collections::VecDeque<(f32, f32)>,
    /// самые прожорливые по памяти программы — считаются в стороне, пока открыт «Монитор»
    procs: Arc<Mutex<Vec<(String, u64)>>>,
    procs_at: Instant,
    /// топ по процессору и прошлый снимок времени процессов, по которому он считается
    procs_cpu: Arc<Mutex<Vec<(String, f32)>>>,
    cpu_snap: Arc<Mutex<Option<(std::collections::HashMap<String, u64>, Instant)>>>,
    /// правая карточка монитора показывает процессор, а не память
    procs_by_cpu: bool,
    /// самообновление: состояние проверки, время следующей и момент установки
    upd: Arc<Mutex<update::State>>,
    upd_at: Instant,
    /// идёт представление обновления: когда началось, какая версия и где её файл (пустой путь — проверочный прогон)
    upd_anim: Option<(Instant, String, std::path::PathBuf)>,
    /// форма острова, которую диктует представление, и его сдвиг по высоте
    upd_shape: Option<Vec2>,
    drop_y: f32,
    /// новая версия «падает» на место после перезапуска
    arrive_at: Option<Instant>,
    arrive_done: bool,
    /// громкость и раскладка из потока наблюдения и то, что уже показано
    watch: Arc<Mutex<sys::Watch>>,
    watch_seen: Option<sys::Watch>,
    clear_saved: u8,
    /// первое знакомство: номер следующей подсказки и когда её показать
    intro: Option<(usize, Instant)>,
    todo_edit: String,
    /// режим «Фокус»: идёт ли перерыв и сколько кругов работы пройдено
    focus: Option<(bool, u32)>,
    /// размеры файлов на полке: None — файла нет, u64::MAX — папка
    shelf_meta: std::collections::HashMap<String, Option<u64>>,
    /// миниатюры картинок на полке: None — читается или не получилось (тогда остаётся значок)
    shelf_thumbs: std::collections::HashMap<String, Option<egui::TextureHandle>>,
    /// готовые картинки из фонового потока
    shelf_thumb_q: Arc<Mutex<Vec<(String, Option<(usize, usize, Vec<u8>)>)>>>,
    mem_hot: u8,
    mem_alert_at: Option<Instant>,
    kill_arm: Option<(String, Instant)>,
    /// скорость сети, байт в секунду: приём и отдача
    net: (f32, f32),
    net_prev: Option<(u64, u64, Instant)>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = &cc.egui_ctx;
        setup_fonts(ctx);
        setup_style(ctx);

        let (media, media_tx) = media::spawn();
        let hotkey = Arc::new(AtomicBool::new(false));
        let hotkey_name = if test_mode() { "Alt + Space" } else { sys::spawn_hotkey(hotkey.clone(), ctx.clone()) };
        let (tx, rx) = mpsc::channel();

        let apps = Arc::new(Mutex::new(Vec::new()));
        {
            let apps = apps.clone();
            std::thread::spawn(move || {
                let list = sys::index_apps();
                *lock(&apps) = list;
            });
        }

        let dl: Arc<Mutex<Option<(u64, f32)>>> = Default::default();
        if std::env::var_os("OSTROVOK_DL").is_some() {
            *dl.lock().unwrap() = Some((36 << 20, 2.4 * 1048576.0));
        }
        if !test_mode() {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            sys::spawn_downloads(
                move |name| {
                    let _ = tx.send(Msg::Download(name));
                    ctx.request_repaint();
                },
                dl.clone(),
            );
        }
        let (lens_tx, lens_rx) = glass::spawn(ctx.clone());
        let fx = cc.gl.as_ref().and_then(|gl| fx::Fx::new(gl)).map(Arc::new);
        let mut cfg = store::Settings::load();
        cfg.world = sanitize_world(&cfg.world);
        THEME.store((cfg.theme as usize).min(THEMES.len() - 1), Ordering::Relaxed);
        if let Some(t) = std::env::var("OSTROVOK_THEME").ok().and_then(|v| v.parse::<usize>().ok()) {
            THEME.store(t.min(THEMES.len() - 1), Ordering::Relaxed);
        }
        let live = {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            live::spawn(cfg.mirror && !test_mode(), move |notice| {
                let _ = tx.send(Msg::Notice(notice));
                ctx.request_repaint();
            })
        };
        let now = Instant::now();
        let mut app = Self {
            mode: Mode::Pill,
            tab: Tab::Home,
            size: vec2(150.0, PILL_H),
            passthrough: None,
            hidden: false,
            outside_since: None,
            last_slow: now,
            emotion: Emotion::Normal,
            emotion_until: now,
            next_blink: now + Duration::from_secs(3),
            blink_until: now,
            note: None,
            timer_end: None,
            timer_custom: "15".into(),
            media,
            media_tx,
            media_now: None,
            thumb: None,
            thumb_gen: 0,
            weather: None,
            weather_at: None,
            weather_err: None,
            weather_fails: 0,
            weather_gen: 0,
            city_req: 0,
            city_edit: cfg.city.clone(),
            jarvis: jarvis::Jarvis::new(jarvis_ui::default_dir(&cfg)),
            ears: ears::Ears::new(jarvis_ui::default_dir(&cfg)),
            setup: setup::Setup::new(jarvis_ui::default_dir(&cfg)),
            jv_view: 0,
            jv_pick: "large",
            voice: voice::Voice::new(jarvis_ui::default_dir(&cfg)),
            cfg,
            autostart: store::autostart_enabled(),
            notes: store::load_notes(),
            note_sel: 0,
            note_query: String::new(),
            notes_dirty: None,
            clipboard: arboard::Clipboard::new().ok(),
            clips: Vec::new(),
            clip_query: String::new(),
            rates: Default::default(),
            rates_shown: false,
            palette_demo: false,
            shot_card: None,
            shot_marks: Vec::new(),
            shot_drag: None,
            clip_seq: sys::clipboard_seq(),
            hotkey,
            hotkey_name,
            tx,
            rx,
            apps,
            query: String::new(),
            results: Vec::new(),
            sel: 0,
            focus_query: false,
            palette_at: now,
            shot: None,
            vel: Vec2::ZERO,
            sticky: false,
            fx,
            bub_timer: Spr::new(0.0, 230.0, 17.0, 0.004),
            bub_call: Spr::new(0.0, 230.0, 17.0, 0.004),
            bub_clip: Spr::new(0.0, 230.0, 17.0, 0.004),
            bub_dl: Spr::new(0.0, 230.0, 17.0, 0.004),
            dl: dl.clone(),
            dl_text: String::new(),
            hint: None,
            hint_until: Instant::now(),
            clip_skip: false,
            bubbles: Vec::new(),
            liq_h: vec![0.0; 72],
            liq_v: vec![0.0; 72],
            level: 0.35,
            last_cx: 0.0,
            pill_rect: Rect::NOTHING,
            timer_total: 60.0,
            timer_paused: None,
            ctl_at: now,
            mixer: Vec::new(),
            sound: sound::Sound::spawn(),
            bars: [0.0; sound::BANDS],
            beat: 0.0,
            sounding: (String::new(), now),
            mixer_at: now.checked_sub(Duration::from_secs(5)).unwrap_or(now),
            quiet_prev: None,
            sheen_at: now,
            hover_was: false,
            vol: None,
            mic: None,
            bin: (0, 0),
            light_theme: None,
            awake: false,
            last_fg: 0,
            album: None,
            live,
            inbox: Vec::new(),
            inbox_filter: None,
            inbox_unread: 0,
            clip_imgs: Vec::new(),
            world_edit: None,
            world_focus: false,
            city_rechecked: false,
            is_installed: install::installed(),
            call_since: None,
            toast: None,
            last_track: String::new(),
            app_icons: std::collections::HashMap::new(),
            jelly: Spr::new(1.0, 420.0, 12.0, 0.0015),
            reach: Spr::new(0.0, 210.0, 17.0, 0.004),
            lean_s: Spr::new(0.0, 190.0, 16.0, 0.03),
            reach_cursor: pos2(WIN_W / 2.0, TOP + PILL_H),
            ripple: 0.0,
            ripple_phase: 0.0,
            drip_phase: 0.0,
            warp: Warp::default(),
            glow: None,
            lens_tx,
            lens_rx,
            lens_tex: None,
            lens_cap: None,
            lens_draw: None,
            lens_key: [0; 5],
            lens_at: now,
            lens_busy: false,
            excluded: None,
            capture_req: None,
            app_query: None,
            confirm: None,
            power_timer: None,
            power_pick: "sleep",
            was_foreground: false,
            qr_text: String::new(),
            qr_cache: None,
            palette_was_open: false,
            qr_wifi: false,
            wifi_ssid: String::new(),
            wifi_pass: String::new(),
            wifi_show: false,
            wifi_secure: true,
            pick_at: None,
            remind_text: String::new(),
            remind_time: String::new(),
            remind_repeat: 0,
            lyrics: Vec::new(),
            lyrics_key: String::new(),
            acrylic: None,
            win_last: None,
            web: None,
            web_ctx: None,
            web_slot: None,
            web_rect: None,
            web_nav: None,
            web_visible: false,
            web_error: None,
            tab_seen: Tab::Home,
            tab_at: now,
            tab_dir: 1.0,
            mode_seen: Mode::Pill,
            mode_at: now,
            ai_chat: Vec::new(),
            ai_input: String::new(),
            ai_busy: false,
            jv_log: Vec::new(),
            jv_msgs: Vec::new(),
            jv_input: String::new(),
            jv_busy: false,
            jv_status: String::new(),
            jv_stats: String::new(),
            jv_confirm: None,
            jv_listen: 0,
            jv_level: 0.0,
            jv_heard: 0.0,
            jv_engine: "",
            jv_attn: None,
            key_edit: String::new(),
            sw_start: None,
            sw_acc: Duration::ZERO,
            cal_shift: 0,
            cal_sel: None,
            cal_text: String::new(),
            cal_time: String::new(),
            meter: sys::CpuMeter::default(),
            stats: sys::Stats::default(),
            stats_at: now,
            load_hist: Default::default(),
            procs: Default::default(),
            procs_cpu: Default::default(),
            cpu_snap: Default::default(),
            procs_by_cpu: false,
            procs_at: now.checked_sub(Duration::from_secs(60)).unwrap_or(now),
            upd: Default::default(),
            upd_at: now + Duration::from_secs(15),
            upd_anim: (test_mode() && std::env::var_os("OSTROVOK_UPD_ANIM").is_some()).then(|| (now + Duration::from_secs(3), "0.1.17".to_string(), std::path::PathBuf::new())),
            upd_shape: None,
            drop_y: 0.0,
            arrive_at: std::env::args().any(|a| a == "--updated").then_some(now),
            arrive_done: false,
            watch: {
                let c = cc.egui_ctx.clone();
                sys::spawn_watch(move || c.request_repaint())
            },
            watch_seen: None,
            clear_saved: 0,
            intro: (test_mode() && std::env::var_os("OSTROVOK_INTRO").is_some()).then_some((0, now)),
            todo_edit: String::new(),
            focus: None,
            shelf_meta: Default::default(),
            shelf_thumbs: Default::default(),
            shelf_thumb_q: Default::default(),
            mem_hot: 0,
            mem_alert_at: None,
            kill_arm: None,
            net: (0.0, 0.0),
            net_prev: None,
        };
        if test_mode() {
            // только для проверки вёрстки: выбрать персонажа и показать уведомление, ничего не сохраняя
            if let Some(c) = std::env::var("OSTROVOK_CHAR").ok().and_then(|v| v.parse::<u8>().ok()) {
                app.cfg.character = c;
            }
            if std::env::var_os("OSTROVOK_TOAST").is_some() {
                app.show_toast("\u{E896}", [0x3A, 0x9B, 0xFF], "Загрузка завершена", "otchet-za-noyabr.pdf", 30, false);
            }
        }
        // для снимков идущего таймера: OSTROVOK_TIMER=25 — таймер на 25 минут, прошло чуть больше трети
        if let Some(m) = std::env::var("OSTROVOK_TIMER").ok().and_then(|v| v.parse::<f64>().ok()).filter(|m| test_mode() && timer_minutes_ok(*m)) {
            app.start_timer(m);
            app.timer_end = Some(Instant::now() + Duration::from_secs_f64(m * 60.0 * 0.62));
            app.note = None;
        }
        // для снимков отсчёта в пилюле: OSTROVOK_SOON=7 — напоминание через 7 минут
        if let Some(m) = std::env::var("OSTROVOK_SOON").ok().and_then(|v| v.parse::<i64>().ok()).filter(|_| test_mode()) {
            app.cfg.reminders.push(store::Reminder { text: "Позвонить".into(), at: chrono::Local::now().timestamp() + m * 60 - 5, repeat: 0 });
        }
        // для снимков недели фокуса: OSTROVOK_FOCUS=30,55,0,80,25,110,50 — минуты за 7 дней, сегодня последним
        if let Some(list) = std::env::var("OSTROVOK_FOCUS").ok().filter(|_| test_mode()) {
            let today = chrono::Local::now().date_naive();
            for (i, m) in list.split(',').filter_map(|v| v.trim().parse::<u32>().ok()).take(7).enumerate() {
                if m > 0 {
                    add_focus(&mut app.cfg.focus_log, today - chrono::Duration::days(6 - i as i64), m);
                }
            }
        }
        // для снимков мировых часов: OSTROVOK_WORLD=Лондон,Токио,Нью-Йорк
        if let Some(list) = std::env::var("OSTROVOK_WORLD").ok().filter(|_| test_mode()) {
            let mut world: Vec<String> = Vec::new();
            for c in list.split(',').filter_map(find_city) {
                if !world.iter().any(|w| w == c.show) && world.len() < WORLD_MAX {
                    world.push(c.show.to_string());
                }
            }
            app.cfg.world = world;
        }
        // для снимков пилюли со значком непрочитанных: OSTROVOK_UNREAD=3
        if let Some(n) = std::env::var("OSTROVOK_UNREAD").ok().and_then(|v| v.parse::<usize>().ok()).filter(|_| test_mode()) {
            for _ in 0..n {
                app.push_inbox(live::Notice { app: "Telegram".into(), title: "Мирон".into(), body: "привет".into() }, [0x2A, 0xAB, 0xEE]);
            }
        }
        // для снимков QR для Wi‑Fi: OSTROVOK_WIFIQR=1 — выдуманная сеть HomeNet / example123
        if test_mode() && std::env::var_os("OSTROVOK_WIFIQR").is_some() {
            app.qr_wifi = true;
            app.wifi_ssid = "HomeNet".into();
            app.wifi_pass = "example123".into();
        }
        // для снимков настраиваемого ряда действий: OSTROVOK_HIDEACT=ocr,cleantext
        if let Some(v) = std::env::var("OSTROVOK_HIDEACT").ok().filter(|_| test_mode()) {
            app.cfg.hidden_actions = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        }
        // для снимков таймера на паузе: OSTROVOK_TIMERPAUSE=25 — таймер на 25 минут, на паузе на трети пути
        if let Some(m) = std::env::var("OSTROVOK_TIMERPAUSE").ok().and_then(|v| v.parse::<f64>().ok()).filter(|m| test_mode() && *m > 0.0 && *m <= 1440.0) {
            app.timer_total = (m * 60.0) as f32;
            app.timer_end = None;
            app.timer_paused = Some(Duration::from_secs_f64(m * 60.0 * 0.62));
        }
        // для снимков повторяющихся напоминаний: OSTROVOK_REPEAT=1 — три повтора и одно разовое
        if test_mode() && std::env::var_os("OSTROVOK_REPEAT").is_some() {
            let now = chrono::Local::now().timestamp();
            for (text, mins, repeat) in [("Сделать перерыв для глаз", 40, 1u8), ("Стендап команды", 300, 2), ("Резервная копия проекта", 1500, 3), ("Забрать посылку", 90, 0)] {
                app.cfg.reminders.push(store::Reminder { text: text.into(), at: now + mins * 60, repeat });
            }
            app.cfg.reminders.sort_by_key(|r| r.at);
            app.remind_repeat = 2;
        }
        // для снимков праздников в календаре: OSTROVOK_CALDAY=11-04 — открыть этот день (ближайший впереди)
        if let Some(v) = std::env::var("OSTROVOK_CALDAY").ok().filter(|_| test_mode()) {
            use chrono::Datelike;
            let today = chrono::Local::now().date_naive();
            if let Some((m, d)) = v.split_once('-').and_then(|(m, d)| Some((m.parse::<u32>().ok()?, d.parse::<u32>().ok()?))) {
                let pick = chrono::NaiveDate::from_ymd_opt(today.year(), m, d).map(|x| if x < today { chrono::NaiveDate::from_ymd_opt(today.year() + 1, m, d).unwrap_or(x) } else { x });
                if let Some(x) = pick {
                    app.cal_sel = Some(x);
                    app.cal_shift = (x.year() * 12 + x.month0() as i32) - (today.year() * 12 + today.month0() as i32);
                }
            }
        }
        // для снимков отложенного сна на «Управлении»: OSTROVOK_POWERTIMER=30 (в тестовом режиме по истечении ничего не делает)
        if let Some(m) = std::env::var("OSTROVOK_POWERTIMER").ok().and_then(|v| v.parse::<u64>().ok()).filter(|_| test_mode()) {
            app.power_timer = Some(("sleep", Instant::now() + Duration::from_secs(power_demo_secs(m)), false));
        }
        // для снимков страницы «Звук» во включённом тихом режиме: OSTROVOK_QUIET=1 (системную громкость не трогает)
        if test_mode() && std::env::var_os("OSTROVOK_QUIET").is_some() {
            app.quiet_prev = Some(0.6);
        }
        // для снимков ленты дня: OSTROVOK_DAY=1 — три дела сегодня (утро, через час, вечер)
        if test_mode() && std::env::var_os("OSTROVOK_DAY").is_some() {
            let now = chrono::Local::now();
            if let Some(mid) = now.date_naive().and_hms_opt(0, 0, 0).and_then(|m| m.and_local_timezone(chrono::Local).earliest()) {
                let m = mid.timestamp();
                let soon = (now.timestamp() + 3600).min(m + 86_340);
                for (text, at) in [("Зарядка", m + 8 * 3600), ("Созвон с командой", soon), ("Полить цветы", m + 21 * 3600 + 1800)] {
                    app.cfg.reminders.push(store::Reminder { text: text.into(), at, repeat: 0 });
                }
            }
        }
        if test_mode() && std::env::var_os("OSTROVOK_TODO").is_some() {
            let today = chrono::Local::now().date_naive();
            app.cfg.todos = [
                ("Обновить драйверы видеокарты", false, Some(-1)),
                ("Сделать резервную копию диска D", false, Some(0)),
                ("Купить SSD на 2 ТБ", false, Some(3)),
                ("Почистить автозагрузку", false, None),
                ("Продлить антивирус", true, Some(-3)),
            ]
            .map(|(t, done, d): (&str, bool, Option<i64>)| store::Todo { text: t.to_string(), done, due: d.map(|d| (today + chrono::Duration::days(d)).format("%Y-%m-%d").to_string()) })
            .to_vec();
        }
        if test_mode() && std::env::var_os("OSTROVOK_SHELF").is_some() {
            app.cfg.shelf = ["C:\\Windows\\Web\\Wallpaper\\Theme1\\img1.jpg", "C:\\Windows\\explorer.exe", "C:\\Windows\\Web\\Wallpaper\\Theme1\\img13.jpg", "C:\\Windows\\System32\\notepad.exe", "C:\\Windows\\win.ini", "C:\\Windows\\Fonts", "C:\\нет\\такого.txt"].map(String::from).to_vec();
        }
        // для снимков: открыть строку поиска с готовым запросом
        if let Ok(q) = std::env::var("OSTROVOK_QUERY") {
            // для снимка пустого поля — выдуманное «Недавнее» вместо настоящего
            if test_mode() {
                app.cfg.recent = ["100 usd в рублях", "время токио", "калькулятор", "таймер 25"].map(String::from).to_vec();
            }
            app.query = q;
            app.build_results();
            app.palette_demo = true;
        }
        if let Ok(t) = std::env::var("OSTROVOK_HINT") {
            app.hint = smart_hint(&t);
            app.hint_until = Instant::now() + Duration::from_secs(60);
        }
        // проверочная копия не трогает файлы обновления настоящего острова
        if !test_mode() {
            update::cleanup();
        }
        if !test_mode() && app.cfg.last_version != env!("CARGO_PKG_VERSION") {
            if !app.cfg.last_version.is_empty() {
                // вторая строка — что нового, если описание релиза сохранилось при загрузке
                let news = update::whats_new().unwrap_or_else(|| "установлена новая версия".to_string());
                app.show_toast("\u{E930}", [0x7B, 0xE0, 0x9A], concat!("Островок обновлён до ", env!("CARGO_PKG_VERSION")), &news, 9, false);
            } else {
                // самый первый запуск: коротко показываем, что остров умеет
                app.intro = Some((0, Instant::now() + Duration::from_secs(2)));
            }
            app.cfg.last_version = env!("CARGO_PKG_VERSION").to_string();
            app.cfg.save();
        }
        if let Ok(demo) = std::env::var("OSTROVOK_DEMO") {
            app.mode = Mode::Panel;
            app.sticky = true;
            app.tab = match demo.as_str() {
                "ai" => Tab::Ai,
                "shot" => {
                    // для снимков: карточка с тем, что сейчас на экране (файл не создаётся)
                    if let Ok((w, h, px)) = sys::capture_screen() {
                        app.show_shot(&cc.egui_ctx, w, h, px, r"C:\Users\user\Pictures\Островок\Снимок 2026-11-16 20-55-10.png".into());
                        // две рамки разметки для вида (OSTROVOK_SHOT_MARKS=1)
                        if std::env::var_os("OSTROVOK_SHOT_MARKS").is_some() {
                            let (w, h) = (w as u32, h as u32);
                            app.shot_marks = vec![[w / 10, h / 8, w * 4 / 10, h * 3 / 8], [w * 55 / 100, h / 2, w * 9 / 10, h * 8 / 10]];
                        }
                    }
                    Tab::Shot
                }
                "notes" => {
                    // для снимков: образцы заметок (в проверочном запуске они не сохраняются)
                    app.notes = ["Перед катками\n- [x] обновить драйвер видеокарты\n- [x] почистить диск C\n- [ ] закрыть лишние вкладки\n- [ ] проверить микрофон в Discord", "Идеи для острова\nгромкость по приложениям колесом\nвиджет загрузки видеокарты", "Сборка ПК\nвзять второй SSD на 1 ТБ\nпоменять термопасту", "Сервер Minecraft\nперезагружать по вторникам"].map(String::from).to_vec();
                    Tab::Notes
                }
                "jarvis" => Tab::Jarvis,
                "jarvis-settings" => {
                    app.jv_view = 1;
                    Tab::Jarvis
                }
                "control" => Tab::Control,
                "inbox" => {
                    // для снимков: выдуманные сообщения (в проверочном запуске уведомления не читаются)
                    let samples = [
                        ("Telegram", "Мирон", "скинь потом ту ссылку на релиз", [0x2A, 0xAB, 0xEE], 2),
                        ("Discord", "#general · Лёша", "кто сегодня в кс? собираем пятёрку к девяти", [0x58, 0x65, 0xF2], 14),
                        ("Telegram", "Steam", "Скидка 70% на игру из вашего списка желаемого", [0x2A, 0xAB, 0xEE], 47),
                        ("WhatsApp", "Доставка", "Курьер будет с 18:00 до 20:00", [0x25, 0xD3, 0x66], 95),
                        ("Discord", "Аня", "смотри что нашла, это же наш остров", [0x58, 0x65, 0xF2], 180),
                    ];
                    for (app_name, title, body, color, mins) in samples.into_iter().rev() {
                        app.push_inbox(live::Notice { app: app_name.into(), title: title.into(), body: body.into() }, color);
                        app.inbox[0].at = chrono::Local::now() - chrono::Duration::minutes(mins);
                    }
                    // для снимка с включённым фильтром: OSTROVOK_INBOX_FILTER=Discord
                    app.inbox_filter = std::env::var("OSTROVOK_INBOX_FILTER").ok().filter(|f| !f.is_empty());
                    Tab::Inbox
                }
                "music" => Tab::Music,
                "remind" => Tab::Remind,
                "qr" => {
                    app.qr_text = "https://qutlawsoasis-debug.github.io/Ostrovok/".into();
                    Tab::Qr
                }
                "shelf" => Tab::Shelf,
                "weather" => Tab::Weather,
                "timer" => Tab::Timer,
                "calendar" => {
                    // для снимков: несколько дел на ближайшие дни
                    let now = chrono::Local::now().timestamp();
                    for (days, hour, text) in [(0, 3, "Созвон с командой"), (0, 5, "Забрать посылку"), (2, 4, "Оплатить интернет"), (6, 2, "День рождения Миши")] {
                        app.cfg.reminders.push(store::Reminder { text: text.into(), at: now + days * 86400 + hour * 3600, repeat: 0 });
                    }
                    Tab::Calendar
                }
                "clip" => {
                    // для снимков: образцы записей, чтобы страница не была пустой
                    app.clips = ["ghbdtn, rfr ltkf?", "https://github.com/qutlawsoasis-debug/Ostrovok", "#7C5CFF", "128 * 4 + 16", "Купить SSD на 2 ТБ и термопасту", r"C:\Windows\System32"].map(String::from).to_vec();
                    // и несколько «снимков» — градиенты разных пропорций
                    for (k, (w, h)) in [(1920usize, 1080usize), (800, 800), (1280, 720), (600, 900)].into_iter().enumerate() {
                        let (a, b) = (hue(0.12 + k as f32 * 0.21, 1.0), hue(0.45 + k as f32 * 0.17, 1.0));
                        let mut px = Vec::with_capacity(w * h * 4);
                        for y in 0..h {
                            for x in 0..w {
                                let c = mix(a, b, (x + y) as f32 / (w + h) as f32);
                                px.extend_from_slice(&[c.r(), c.g(), c.b(), 255]);
                            }
                        }
                        app.push_clip_img(&cc.egui_ctx, w, h, px);
                    }
                    Tab::Clip
                }
                "todo" => Tab::Todo,
                "system" => Tab::System,
                "system-cpu" => {
                    app.procs_by_cpu = true;
                    Tab::System
                }
                "sound" => Tab::Sound,
                "settings" => Tab::Settings,
                _ => Tab::Home,
            };
            app.tab_seen = app.tab;
        }
        app
    }

    fn notify(&mut self, text: &str) {
        let mut t: String = text.chars().take(52).collect();
        if text.chars().count() > 52 {
            t.push('…');
        }
        self.note = Some((t, Instant::now() + Duration::from_secs(4)));
        self.ripple = (self.ripple + 4.0).min(7.0);
        self.glow = Some((accent(), Instant::now()));
    }

    fn show_toast(&mut self, glyph: &'static str, color: [u8; 3], title: &str, text: &str, secs: u64, cover: bool) {
        let short = |v: &str, n: usize| -> String {
            let mut out: String = v.chars().take(n).collect();
            if v.chars().count() > n {
                out.push('…');
            }
            out
        };
        self.toast = Some(Toast { glyph, color, title: short(title, 44), text: short(text, 56), until: Instant::now() + Duration::from_secs(secs), cover, level: None, compact: false });
        self.ripple = (self.ripple + 4.0).min(7.0);
        self.jelly.vel += 0.7;
        self.glow = Some((Color32::from_rgb(color[0], color[1], color[2]), Instant::now()));
        self.emotion = Emotion::Happy;
        self.emotion_until = Instant::now() + Duration::from_millis(1600);
    }

    /// Маленькая всплывашка в пилюле (громкость, раскладка). Важное уведомление она не перебивает.
    fn show_hud(&mut self, glyph: &'static str, color: [u8; 3], title: String, level: Option<f32>, ms: u64) {
        if self.toast.as_ref().is_some_and(|t| !t.compact) {
            return;
        }
        self.toast = Some(Toast { glyph, color, title, text: String::new(), until: Instant::now() + Duration::from_millis(ms), cover: false, level, compact: true });
    }

    fn start_timer(&mut self, minutes: f64) {
        if !timer_minutes_ok(minutes) {
            return;
        }
        self.timer_end = Some(Instant::now() + Duration::from_secs_f64(minutes * 60.0));
        self.timer_total = (minutes * 60.0) as f32;
        self.timer_paused = None;
        self.notify(&format!("Таймер на {} мин", fmt_num(minutes)));
    }

    fn set_clipboard(&mut self, text: &str) {
        self.clip_skip = true;
        if let Some(cb) = &mut self.clipboard {
            let _ = cb.set_text(text.to_string());
        }
    }

    fn toggle_palette(&mut self, ctx: &egui::Context) {
        if self.mode == Mode::Palette {
            self.mode = Mode::Pill;
            return;
        }
        self.mode = Mode::Palette;
        self.query.clear();
        self.results.clear();
        self.sel = 0;
        self.focus_query = true;
        self.palette_at = Instant::now();
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }

    fn refresh_weather(&mut self, ctx: &egui::Context) {
        self.weather_at = Some(Instant::now());
        let (Some(lat), Some(lon)) = (self.cfg.lat, self.cfg.lon) else { return };
        let gen = self.weather_gen;
        // координаты, сохранённые старой версией, могли указать на одноимённый город за океаном
        // (Минден в США вместо Германии). Один раз за запуск переспрашиваем город с учётом часового пояса.
        if !self.city_rechecked && !self.cfg.city_checked && !self.cfg.city.trim().is_empty() && !weather::fits_zone(lon, weather::local_offset_hours()) {
            self.city_rechecked = true;
            // проверяем один раз за всю жизнь настроек: город, выбранный вручную после этого, не трогаем
            self.cfg.city_checked = true;
            self.cfg.save();
            let req = self.city_req;
            let (tx, ctx, city) = (self.tx.clone(), ctx.clone(), self.cfg.city.clone());
            std::thread::spawn(move || {
                if let Ok(Some((la, lo, name))) = weather::geocode(&city) {
                    if weather::fits_zone(lo, weather::local_offset_hours()) {
                        let _ = tx.send(Msg::City(gen, req, la, lo, name, city));
                        ctx.request_repaint();
                    }
                }
            });
        }
        let (tx, ctx, name) = (self.tx.clone(), ctx.clone(), self.cfg.city_name.clone());
        std::thread::spawn(move || {
            let _ = tx.send(match weather::fetch(lat, lon, &name) {
                Ok(w) => Msg::Weather(gen, w),
                Err(e) => Msg::WeatherFailed(gen, e),
            });
            ctx.request_repaint();
        });
    }

    fn save_city(&mut self, ctx: &egui::Context) {
        self.city_req += 1;
        let req = self.city_req;
        // ответ о новом городе придёт с поколением +1 и только тогда сделает прежние запросы устаревшими:
        // если город не найдётся, текущая погода и её повторы продолжают работать
        let gen = self.weather_gen + 1;
        let city = self.city_edit.trim().to_string();
        if city.is_empty() {
            self.weather_gen = gen;
            self.weather_err = None;
            self.weather_fails = 0;
            self.cfg.city.clear();
            self.cfg.city_name.clear();
            self.cfg.lat = None;
            self.cfg.lon = None;
            self.cfg.save();
            self.weather = None;
            self.notify("Погода отключена");
            return;
        }
        // название сохраним, только когда геокодер найдёт город: иначе остаётся прежнее
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let msg = match weather::geocode(&city) {
                Ok(Some((lat, lon, name))) => Msg::City(gen, req, lat, lon, name, city.clone()),
                Ok(None) => Msg::CityFailed(req, "Город не найден".into()),
                Err(_) => Msg::CityFailed(req, "Нет связи с сервером погоды".into()),
            };
            let _ = tx.send(msg);
            ctx.request_repaint();
        });
    }

    /// Запускает системное выделение области, ждёт снимок в буфере и распознаёт на нём текст.
    fn ocr_from_snip(&mut self, ctx: &egui::Context) {
        let seq0 = sys::clipboard_seq();
        sys::open("ms-screenclip:");
        self.mode = Mode::Pill;
        self.notify("Выделите область с текстом");
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let start = Instant::now();
            loop {
                std::thread::sleep(Duration::from_millis(300));
                if start.elapsed() > Duration::from_secs(60) {
                    return;
                }
                if sys::clipboard_seq() == seq0 {
                    continue;
                }
                std::thread::sleep(Duration::from_millis(250));
                let res = (|| -> Result<usize, String> {
                    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
                    let img = cb.get_image().map_err(|_| "В буфере нет снимка".to_string())?;
                    let text = sys::ocr(img.width, img.height, &img.bytes)?;
                    if text.is_empty() {
                        return Err("Текст не найден".into());
                    }
                    let n = text.chars().count();
                    cb.set_text(text).map_err(|e| e.to_string())?;
                    Ok(n)
                })();
                let _ = tx.send(Msg::Note(match res {
                    Ok(n) => format!("Текст скопирован: {n} симв."),
                    Err(e) => e,
                }));
                ctx.request_repaint();
                return;
            }
        });
    }

    fn build_results(&mut self) {
        self.results.clear();
        let q = self.query.trim().to_string();
        if q.is_empty() {
            return;
        }
        let low = q.to_lowercase();

        if let Some(cmd) = parse_quick(&q, chrono::Local::now().naive_local()) {
            let today = chrono::Local::now().date_naive();
            let title = match &cmd {
                Quick::Todo(t) => format!("Добавить дело: {t}"),
                Quick::Note(t) => format!("Новая заметка: {t}"),
                Quick::Remind(t, at) => {
                    let day = if at.date() == today { "сегодня".to_string() } else if at.date() == today.succ_opt().unwrap_or(today) { "завтра".to_string() } else { at.format("%d.%m").to_string() };
                    format!("Напомнить {day} в {}: {t}", at.format("%H:%M"))
                }
            };
            self.results.push(Item { title, sub: "Enter".into(), action: Action::Quick(cmd) });
        }

        // счёт дней: «через 45 дней», «до нового года», «15.02.2027» (быстрые команды важнее)
        let mut date_items = 0;
        if self.results.is_empty() {
            if let Some((d, title)) = date_query(&q, chrono::Local::now().date_naive()) {
                self.results.push(Item { title, sub: "скопировать".into(), action: Action::Copy(d.format("%d.%m.%Y").to_string()) });
                date_items = 1;
            }
        }

        // «время токио» — местное время в городе
        for prefix in ["время ", "time "] {
            if let Some(city) = low.strip_prefix(prefix).and_then(find_city) {
                let utc = chrono::Utc::now().naive_utc();
                let there = utc + chrono::Duration::minutes(city_offset(city, utc) as i64);
                let t = there.format("%H:%M").to_string();
                self.results.push(Item {
                    title: format!("{} — {t} ({})", city.show, world_label(chrono::Local::now().naive_local(), there)),
                    sub: "скопировать".into(),
                    action: Action::Copy(format!("{} {t}", city.show)),
                });
            }
        }

        for prefix in ["таймер ", "timer "] {
            if let Some(rest) = low.strip_prefix(prefix) {
                if let Some(m) = rest.trim().replace(',', ".").parse::<f64>().ok().filter(|m| timer_minutes_ok(*m)) {
                    self.results.push(Item {
                        title: format!("Таймер на {} мин", fmt_num(m)),
                        sub: "запустить".into(),
                        action: Action::Timer(m),
                    });
                }
            }
        }

        // конвертер: единицы считаются сразу, для валют подгружаются курсы
        let rates = self.rates.get();
        for (title, value, sub) in convert::convert(&q, rates.as_ref()) {
            // курсов нет и загрузить не вышло — говорим честно, а не «загружается» до бесконечности
            let title = if rates.is_none() && value.is_empty() && self.rates.failed() { "Нет связи — курсы валют недоступны, попробую позже".to_string() } else { title };
            let action = if value.is_empty() { Action::Copy(String::new()) } else { Action::Copy(value) };
            self.results.push(Item { title, sub: sub.into(), action });
        }
        // ответ про дату не прячет «Спросить ИИ» (быстрые команды и конвертер — прячут)
        let converted = self.results.len() > date_items;

        let is_expr = q.chars().all(|c| c.is_ascii_digit() || " +-*/().,".contains(c))
            && q.chars().any(|c| c.is_ascii_digit())
            && q.chars().any(|c| "+-*/".contains(c));
        if is_expr {
            if let Some(v) = sys::eval(&q) {
                let s = fmt_num(v);
                self.results.push(Item { title: format!("= {s}"), sub: "скопировать".into(), action: Action::Copy(s) });
            }
        }

        {
            let apps = lock(&self.apps);
            let rank = |low: &str, bias: u8| {
                let low = low.to_string();
                move |a: &(String, String)| {
                    let n = a.0.to_lowercase();
                    if n.starts_with(&low) {
                        Some(bias)
                    } else if n.contains(&format!(" {low}")) {
                        Some(bias + 1)
                    } else if n.contains(&low) {
                        Some(bias + 2)
                    } else {
                        None
                    }
                }
            };
            let direct = rank(&low, 0);
            // набрали в другой раскладке («тщеузфв» вместо «notepad») — ищем и так, но ниже прямых совпадений
            let swapped = swap_layout(&low);
            let other = rank(&swapped, 3);
            let mut found: Vec<(u8, &(String, String))> = apps
                .iter()
                .filter_map(|a| direct(a).or_else(|| if swapped != low && swapped.chars().count() >= 2 { other(a) } else { None }).map(|r| (r, a)))
                .collect();
            found.sort_by_key(|(r, a)| (*r, a.0.len()));
            for (_, a) in found.into_iter().take(5) {
                self.results.push(Item { title: a.0.clone(), sub: "программа".into(), action: Action::Launch(a.1.clone()) });
            }
        }

        let looks_url = !q.contains(' ') && q.contains('.') && !q.ends_with('.') && !is_expr;
        if looks_url {
            let url = if q.starts_with("http://") || q.starts_with("https://") { q.clone() } else { format!("https://{q}") };
            self.results.push(Item { title: format!("Открыть {q}"), sub: "сайт".into(), action: Action::Url(url) });
        }
        if !is_expr && !converted {
            self.results.push(Item { title: format!("Спросить ИИ: {q}"), sub: "чат".into(), action: Action::Ask(q.clone()) });
        }
        self.results.push(Item {
            title: format!("Искать «{q}» в интернете"),
            sub: "Google".into(),
            action: Action::Url(format!("https://www.google.com/search?q={}", weather::encode(&q))),
        });
        self.results.truncate(7);
    }

    /// Не даёт кэшу значков расти без конца; заодно сбрасывает неудачные (None) — попробуем снова.
    fn prune_icons(&mut self) {
        self.app_icons.retain(|_, v| v.is_some());
        let apps = lock(&self.apps);
        let mut keep: std::collections::HashSet<&str> = apps.iter().map(|a| a.1.as_str()).collect();
        keep.extend(self.cfg.shelf.iter().map(String::as_str));
        keep.extend(self.cfg.apps.iter().map(|a| a.path.as_str()));
        keep.extend(self.mixer.iter().map(|a| a.path.as_str()));
        keep.extend(self.results.iter().filter_map(|r| if let Action::Launch(p) = &r.action { Some(p.as_str()) } else { None }));
        let gone = icon_keys_to_drop(self.app_icons.keys().map(String::as_str), &keep, ICON_LIMIT);
        drop(apps);
        for k in gone {
            self.app_icons.remove(&k);
        }
    }

    fn run(&mut self, action: Action) {
        self.mode = Mode::Pill;
        match action {
            Action::Launch(p) if !p.starts_with("\\\\") && !std::path::Path::new(&p).exists() => {
                // ярлык удалили после индексации — убираем его из поиска
                lock(&self.apps).retain(|a| a.1 != p);
                self.app_icons.remove(&p);
                self.notify("Ярлык удалён");
            }
            Action::Launch(p) | Action::Url(p) => sys::open(&p),
            Action::Copy(s) if s.is_empty() => {}
            Action::Copy(s) => {
                self.set_clipboard(&s);
                self.notify(&format!("Скопировано: {s}"));
            }
            Action::Timer(m) => self.start_timer(m),
            Action::Ask(q) if self.api_key().is_empty() => self.open_ai_site(&q),
            Action::Ask(q) => {
                self.mode = Mode::Panel;
                self.tab = Tab::Ai;
                self.sticky = true;
                self.ask(q);
            }
            Action::Quick(Quick::Todo(text)) => {
                self.cfg.todos.insert(0, new_todo(&text));
                self.cfg.todos.truncate(60);
                self.cfg.save();
                self.notify("Дело добавлено");
            }
            Action::Quick(Quick::Note(text)) => {
                // пустую заметку в начале списка заменяем, иначе добавляем новую сверху
                if self.notes.first().is_some_and(|n| n.trim().is_empty()) {
                    self.notes[0] = text;
                } else {
                    self.notes.insert(0, text);
                }
                self.note_sel = 0;
                self.notes_dirty = Some(Instant::now());
                self.notify("Заметка сохранена");
            }
            Action::Quick(Quick::Remind(text, at)) => {
                // в ночь перевода часов 02:30 бывает дважды (берём первое) или не бывает вовсе (03:30)
                match resolve_here(at) {
                    Some(t) => {
                        self.cfg.reminders.push(store::Reminder { text, at: t.timestamp(), repeat: 0 });
                        self.cfg.reminders.sort_by_key(|r| r.at);
                        self.cfg.save();
                        self.notify(&format!("Напомню в {}", at.format("%H:%M")));
                    }
                    None => self.notify("Не понял время"),
                }
            }
        }
    }

    /// Фоновые дела: сообщения потоков, буфер обмена, таймер, погода, заметки.
    fn pump(&mut self, ctx: &egui::Context, now: Instant) {
        if self.palette_demo {
            self.mode = Mode::Palette;
            self.sticky = true;
        }
        // столбики эквалайзера догоняют звук плавно, без дрожания
        let snd = self.sound.frame();
        for (b, t) in self.bars.iter_mut().zip(snd.bands) {
            *b += (t - *b) * if t > *b { 0.6 } else { 0.25 };
        }
        self.beat += (snd.bass - self.beat) * 0.4;
        // постоянное прослушивание включается и выключается по настройке
        if !test_mode() && self.cfg.jarvis_listen != self.ears.watching() {
            if self.cfg.jarvis_listen {
                let (tx, c) = (self.tx.clone(), ctx.clone());
                self.ears.watch(self.voice.speaking(), move |h| {
                    let _ = tx.send(Msg::Heard(h));
                    c.request_repaint();
                });
            } else {
                self.ears.unwatch();
            }
        }
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Note(t) => self.notify(&t),
                Msg::Weather(gen, mut w) => {
                    if !weather::reply_fresh(gen, self.weather_gen) {
                        continue;
                    }
                    if test_mode() {
                        if let Ok(city) = std::env::var("OSTROVOK_CITY") {
                            w.city = city;
                        }
                    }
                    self.weather = Some(w);
                    self.weather_err = None;
                    self.weather_fails = 0;
                }
                Msg::WeatherFailed(gen, e) => {
                    if !weather::reply_fresh(gen, self.weather_gen) {
                        continue;
                    }
                    self.weather_err = Some(e);
                    self.weather_fails = self.weather_fails.saturating_add(1);
                }
                Msg::Notice(n) => {
                    // цвет значка — по мессенджеру
                    let low = n.app.to_lowercase();
                    let color = if low.contains("discord") {
                        [0x58, 0x65, 0xF2]
                    } else if low.contains("telegram") {
                        [0x2A, 0xAB, 0xEE]
                    } else if low.contains("whatsapp") {
                        [0x25, 0xD3, 0x66]
                    } else if low.contains("viber") {
                        [0x7D, 0x3D, 0xAF]
                    } else {
                        [0x3A, 0x9B, 0xFF]
                    };
                    let body = if n.body.is_empty() { n.app.clone() } else { n.body.clone() };
                    self.show_toast("\u{E8BD}", color, &n.title, &body, 5, false);
                    self.push_inbox(n, color);
                }
                Msg::Download(name) => self.show_toast("\u{E896}", [0x3A, 0x9B, 0xFF], "Загрузка завершена", &name, 6, false),
                Msg::ClipImg(w, h, px) => self.push_clip_img(ctx, w, h, px),
                Msg::Icon(path, icon) => {
                    let tex = icon.map(|(w, h, px)| ctx.load_texture(format!("pal-{path}"), egui::ColorImage::from_rgba_unmultiplied([w, h], &px), egui::TextureOptions::LINEAR));
                    self.app_icons.insert(path, tex);
                }
                Msg::Lyrics(key, lines) => {
                    if key == self.lyrics_key {
                        self.lyrics = lines;
                    }
                }
                Msg::Shot(w, h, px) => match sys::save_png(w, h, &px) {
                    Ok(path) => self.show_shot(ctx, w, h, px, path),
                    Err(e) => self.notify(&format!("Ошибка: {e}")),
                },
                Msg::Jarvis(ev) => self.jarvis_event(ev),
                Msg::Heard(h) => self.jarvis_heard(h, ctx),
                Msg::Ai(res) => {
                    self.ai_busy = false;
                    match res {
                        Ok(text) => self.ai_chat.push((false, text)),
                        Err(e) => {
                            // вопрос без ответа убираем из истории, чтобы роли в переписке чередовались
                            if self.ai_chat.last().is_some_and(|m| m.0) {
                                if let Some((_, q)) = self.ai_chat.pop() {
                                    self.ai_input = q;
                                }
                            }
                            self.notify(&e);
                        }
                    }
                }
                Msg::CityFailed(req, text) => {
                    if req == self.city_req {
                        self.notify(&text);
                    }
                }
                Msg::City(gen, req, lat, lon, name, query) => {
                    if !weather::city_reply_accept(gen, self.weather_gen, req, self.city_req) {
                        continue;
                    }
                    if gen != self.weather_gen {
                        // принят новый город: свежий счётчик сбоев, без чужой ошибки
                        self.weather_gen = gen;
                        self.weather_err = None;
                        self.weather_fails = 0;
                    }
                    apply_city(&mut self.cfg, &query, lat, lon, &name);
                    self.cfg.save();
                    self.notify(&format!("Город: {name}"));
                    self.refresh_weather(ctx);
                }
            }
        }
        if self.hotkey.swap(false, Ordering::Relaxed) {
            self.toggle_palette(ctx);
        }
        if self.toast.as_ref().is_some_and(|t| now >= t.until) {
            self.toast = None;
        }
        // новый трек — живое уведомление «сейчас играет»
        if let Some(m) = &self.media_now {
            let key = format!("{}|{}", m.artist, m.title);
            if m.playing && key != self.last_track {
                let first = self.last_track.is_empty();
                self.last_track = key;
                if !first && self.mode == Mode::Pill {
                    let (title, artist) = (m.title.clone(), m.artist.clone());
                    self.show_toast("\u{E8D6}", [185, 164, 255], &title, &artist, 4, true);
                }
            }
        }
        if self.note.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.note = None;
        }
        if self.emotion != Emotion::Normal && now >= self.emotion_until {
            self.emotion = Emotion::Normal;
        }
        if now >= self.next_blink {
            self.blink_until = now + Duration::from_millis(130);
            self.next_blink = now + Duration::from_millis(2500 + (now.elapsed().subsec_nanos() as u64 + self.clip_seq as u64 * 977) % 3500);
        }

        {
            let st = self.media.lock().unwrap();
            self.media_now = st.info.clone();
            // для снимков страницы проекта: название того, что играет у автора, в кадр не попадает
            if test_mode() && std::env::var_os("OSTROVOK_NOMEDIA").is_some() {
                self.media_now = None;
            }
            // проверка вида пилюли без плеера: OSTROVOK_TRACK=«название»
            if let (None, Ok(t)) = (&self.media_now, std::env::var("OSTROVOK_TRACK")) {
                self.media_now = Some(media::Info { title: media::clean_title(t.split(" — ").next().unwrap_or(&t)), artist: t.split_once(" — ").map_or("Исполнитель".into(), |p| p.1.to_string()), playing: true, pos: 64.0, dur: 200.0, at: Instant::now() });
            }
            if st.thumb_gen != self.thumb_gen {
                self.thumb_gen = st.thumb_gen;
                self.album = st.accent;
                self.thumb = st.thumb.as_ref().map(|(w, h, px)| {
                    ctx.load_texture("cover", egui::ColorImage::from_rgba_unmultiplied([*w, *h], px), egui::TextureOptions::LINEAR)
                });
            }
        }

        if self.cfg.lyrics {
            if let Some(m) = &self.media_now {
                let key = format!("{}|{}", m.artist, m.title);
                if key != self.lyrics_key {
                    self.lyrics_key = key.clone();
                    self.lyrics.clear();
                    let (tx, artist, title) = (self.tx.clone(), m.artist.clone(), m.title.clone());
                    std::thread::spawn(move || {
                        if let Some(lines) = media::fetch_lyrics(&artist, &title) {
                            let _ = tx.send(Msg::Lyrics(key, lines));
                        }
                    });
                }
            }
        }

        // проверка вида всплывашек на тестовой копии: OSTROVOK_HUD=vol или lang
        if test_mode() && self.toast.is_none() {
            match std::env::var("OSTROVOK_HUD").as_deref() {
                Ok("vol") => self.show_hud("\u{E767}", [185, 164, 255], String::new(), Some(0.64), 60_000),
                Ok("lang") => self.show_hud("\u{E765}", [0x8F, 0xD3, 0xFF], "Русский".to_string(), None, 60_000),
                _ => {}
            }
        }
        // громкость и раскладка всплывают в пилюле, когда меняются
        let w = *self.watch.lock().unwrap();
        if let Some(prev) = self.watch_seen.replace(w) {
            if !self.cfg.no_hud && self.mode == Mode::Pill && !test_mode() {
                if w.ok && prev.ok && ((w.vol - prev.vol).abs() > 0.004 || w.muted != prev.muted) {
                    let a = accent();
                    let quiet = w.muted || w.vol < 0.005;
                    self.show_hud(if quiet { "\u{E74F}" } else { "\u{E767}" }, [a.r(), a.g(), a.b()], String::new(), Some(if w.muted { 0.0 } else { w.vol }), 1400);
                } else if w.layout != prev.layout && w.layout != 0 && prev.layout != 0 {
                    let name = match w.layout {
                        0x419 => "Русский".to_string(),
                        0x409 | 0x809 => "English".to_string(),
                        0x422 => "Українська".to_string(),
                        0x423 => "Беларуская".to_string(),
                        0x43F => "Қазақша".to_string(),
                        0x407 => "Deutsch".to_string(),
                        other => format!("Раскладка {other:04X}"),
                    };
                    self.show_hud("\u{E765}", [0x8F, 0xD3, 0xFF], name, None, 1100);
                }
            }
        }

        if now.duration_since(self.last_slow) < Duration::from_millis(400) {
            return;
        }
        self.last_slow = now;

        // самообновление: проверка раз в минуту, установка — когда остров свёрнут и его не трогают
        let upd_on = !self.cfg.no_update && (!test_mode() || std::env::var_os("OSTROVOK_FAKE_VERSION").is_some());
        if upd_on && now >= self.upd_at {
            self.upd_at = now + Duration::from_secs(60);
            let c = ctx.clone();
            update::spawn(self.upd.clone(), move || c.request_repaint());
        }
        let ready = match &*self.upd.lock().unwrap() {
            update::State::Ready(v, p) => Some((v.clone(), p.clone())),
            _ => None,
        };
        if let (Some((version, path)), true) = (ready, upd_on) {
            // во время игры или другого полноэкранного приложения остров не перезапускается:
            // новое окно может отобрать у игры фокус, и она свернётся. Обновление подождёт.
            if self.upd_anim.is_none() && self.mode == Mode::Pill && !self.hover_was && self.toast.is_none() && !self.hidden && !sys::fullscreen_app_active() {
                self.upd_anim = Some((now, version, path));
            }
        }

        if let Some((step, at)) = self.intro {
            const STEPS: [(&str, &str, &str); 5] = [
                ("\u{E76E}", "Привет! Это Островок", "щёлкните по нему — раскроется панель"),
                ("\u{E767}", "Колесо мыши над островом", "меняет громкость"),
                ("\u{E721}", "Alt + Пробел", "поиск программ, примеры и конвертер"),
                ("\u{E8B7}", "Перетащите файл на остров", "он останется на полке под рукой"),
                ("\u{E713}", "Темы, персонаж и прозрачность", "в настройках — значок шестерёнки"),
            ];
            if now >= at && self.toast.is_none() && self.mode == Mode::Pill {
                match STEPS.get(step) {
                    Some((glyph, title, text)) => {
                        let a = accent();
                        self.show_toast(glyph, [a.r(), a.g(), a.b()], title, text, 5, false);
                        self.intro = Some((step + 1, now + Duration::from_millis(5600)));
                    }
                    None => self.intro = None,
                }
            }
        }

        let seq = sys::clipboard_seq();
        if seq != self.clip_seq {
            self.clip_seq = seq;
            let text = self.clipboard.as_mut().map(|cb| cb.get_text());
            if !matches!(text, Some(Ok(_))) {
                // в буфере не текст — возможно, картинка (снимок экрана, «копировать изображение»)
                if !std::mem::take(&mut self.clip_skip) {
                    // большой снимок (4K — десятки мегабайт) читаем в фоне, чтобы интерфейс не дёргался
                    let (tx, ctx2) = (self.tx.clone(), ctx.clone());
                    std::thread::spawn(move || {
                        let Ok(mut cb) = arboard::Clipboard::new() else { return };
                        let Ok(img) = cb.get_image() else { return };
                        if img.width > 0 && img.height > 0 && img.bytes.len() < 40_000_000 {
                            let _ = tx.send(Msg::ClipImg(img.width, img.height, img.bytes.into_owned()));
                            ctx2.request_repaint();
                        }
                    });
                }
            }
            if let Some(Ok(t)) = text {
                if !t.trim().is_empty() && t.len() < 200_000 {
                    // своё же копирование подсказку не вызывает
                    if std::mem::take(&mut self.clip_skip) {
                    } else if let Some(h) = smart_hint(&t) {
                        self.hint = Some(h);
                        self.hint_until = now + Duration::from_secs(8);
                    }
                    self.clips.retain(|c| c != &t);
                    self.clips.insert(0, t);
                    self.clips.truncate(30);
                }
            }
        }

        if self.timer_end.is_some_and(|end| now >= end) {
            self.timer_end = None;
            sys::beep();
            match self.focus {
                // работа кончилась — перерыв; перерыв кончился — снова работа
                Some((false, rounds)) => {
                    self.log_focus(25);
                    self.focus = Some((true, rounds + 1));
                    self.start_timer(5.0);
                    self.show_toast("\u{E916}", [0x30, 0xD1, 0x58], "Перерыв 5 минут", &format!("круг {} пройден — отдохните", rounds + 1), 6, false);
                }
                Some((true, rounds)) => {
                    self.focus = Some((false, rounds));
                    self.start_timer(25.0);
                    self.show_toast("\u{E916}", [0xFF, 0xB3, 0x40], "Фокус 25 минут", "перерыв окончен — за работу", 6, false);
                }
                None => self.show_toast("\u{E916}", [0x30, 0xD1, 0x58], "Таймер завершён", "Время вышло", 8, false),
            }
            self.emotion = Emotion::Surprised;
            self.emotion_until = now + Duration::from_secs(4);
        }

        if let Some(at) = self.pick_at {
            if now >= at {
                self.pick_at = None;
                match sys::pixel_at_cursor() {
                    Some(hex) => {
                        self.set_clipboard(&hex);
                        self.notify(&format!("Цвет {hex} скопирован"));
                    }
                    None => self.notify("Не удалось прочитать цвет"),
                }
            } else {
                let left = at.duration_since(now).as_secs_f32().ceil() as u32;
                self.note = Some((format!("Пипетка: наведите курсор… {left}"), now + Duration::from_secs(1)));
            }
        }

        let stamp = chrono::Local::now().timestamp();
        if self.cfg.reminders.iter().any(|r| r.at <= stamp) {
            let due: Vec<String> = self.cfg.reminders.iter().filter(|r| r.at <= stamp).map(|r| r.text.clone()).collect();
            // повторяющиеся переносим на следующий срок, разовые удаляем
            let now_local = chrono::DateTime::from_timestamp(stamp, 0).map_or_else(|| chrono::Local::now().naive_local(), |d| d.with_timezone(&chrono::Local).naive_local());
            for r in self.cfg.reminders.iter_mut().filter(|r| r.at <= stamp && r.repeat != 0) {
                let next = chrono::DateTime::from_timestamp(r.at, 0)
                    .and_then(|d| reschedule(d.with_timezone(&chrono::Local).naive_local(), r.repeat, now_local, stamp, |x| {
                        use chrono::TimeZone;
                        chrono::Local.from_local_datetime(&x)
                    }));
                match next {
                    Some(n) => r.at = n,
                    None => r.repeat = 0,
                }
            }
            self.cfg.reminders.retain(|r| r.at > stamp);
            self.cfg.reminders.sort_by_key(|r| r.at);
            self.cfg.save();
            sys::beep();
            self.show_toast("\u{E823}", [0xFF, 0x6B, 0x61], "Напоминание", &due.join("; "), 12, false);
            self.emotion = Emotion::Surprised;
            self.emotion_until = now + Duration::from_secs(4);
        }

        let in_call = self.live.discord.load(Ordering::Relaxed);
        match (in_call, self.call_since) {
            (true, None) => {
                self.call_since = Some(now);
                // звонок, который уже шёл при запуске, не перебивает карточку «обновлён»
                if ctx.input(|i| i.time) > 12.0 {
                    self.show_toast("\u{E720}", [0x58, 0x65, 0xF2], "Звонок в Discord", "Микрофон используется", 4, false);
                }
            }
            (false, Some(since)) => {
                self.call_since = None;
                let secs = now.duration_since(since).as_secs();
                self.show_toast("\u{E720}", [0x58, 0x65, 0xF2], "Звонок завершён", &format!("Длился {:02}:{:02}", secs / 60, secs % 60), 4, false);
            }
            _ => {}
        }

        if self.cfg.lat.is_some() && self.weather_at.map_or(true, |t| now.duration_since(t) > weather::retry_after(self.weather_fails)) {
            self.refresh_weather(ctx);
        }

        if self.notes_dirty.is_some_and(|t| now.duration_since(t) > Duration::from_secs(1)) {
            store::save_notes(&self.notes);
            self.notes_dirty = None;
        }

        self.hidden = !test_mode() && !self.cfg.over_games && sys::fullscreen_app_active();
        let fg = sys::foreground_other();
        if fg != 0 {
            self.last_fg = fg;
        }
        if now.duration_since(self.stats_at) >= Duration::from_millis(1500) {
            self.stats_at = now;
            // пароль Wi‑Fi живёт только пока открыта страница QR
            if !(self.mode == Mode::Panel && self.tab == Tab::Qr) && !self.wifi_pass.is_empty() {
                self.wifi_pass.clear();
                self.wifi_show = false;
            }
            // отложенная блокировка или сон со страницы «Управление»
            if let Some((action, at, warned)) = self.power_timer {
                let left = at.saturating_duration_since(now).as_secs();
                let (fire, warn) = power_due(left, warned);
                if fire {
                    self.power_timer = None;
                    if !test_mode() {
                        store::save_notes(&self.notes);
                        sys::power(action);
                    }
                } else if warn {
                    self.power_timer = Some((action, at, true));
                    self.notify(if action == "sleep" { "Сон через минуту" } else { "Блокировка через минуту" });
                }
            }
            self.stats = self.meter.sample();
            // для снимков батарейки в пилюле: OSTROVOK_BATTERY=8 (разряжается) или 15c (заряжается)
            if let Some(v) = std::env::var("OSTROVOK_BATTERY").ok().filter(|_| test_mode()) {
                let charging = v.ends_with('c');
                if let Ok(pct) = v.trim_end_matches('c').parse::<u8>() {
                    self.stats.battery = Some((pct.min(100), charging));
                }
            }
            if let Some((rx, tx)) = sys::net_bytes() {
                if let Some((prx, ptx, at)) = self.net_prev {
                    let dt = now.duration_since(at).as_secs_f32().max(0.2);
                    self.net = (rx.saturating_sub(prx) as f32 / dt, tx.saturating_sub(ptx) as f32 / dt);
                }
                self.net_prev = Some((rx, tx, now));
            }
            let mem = if self.stats.mem_total > 0.0 { (self.stats.mem_used / self.stats.mem_total) as f32 } else { 0.0 };
            self.load_hist.push_back((self.stats.cpu, mem));
            // память забита несколько замеров подряд — предупреждаем, но не чаще раза в четверть часа
            self.mem_hot = if mem > 0.92 { self.mem_hot.saturating_add(1) } else { 0 };
            if self.mem_hot == 4 && self.mem_alert_at.map_or(true, |t| now.duration_since(t) > Duration::from_secs(900)) {
                self.mem_alert_at = Some(now);
                self.show_toast("\u{E7BA}", [0xFF, 0x6B, 0x6B], "Память почти заполнена", &format!("занято {:.0}% — загляните в «Монитор»", mem * 100.0), 7, false);
            }
            while self.load_hist.len() > 120 {
                self.load_hist.pop_front();
            }
        }

    }

    fn segments(&self) -> Vec<Seg> {
        let mut v = Vec::new();
        if let Some((t, _)) = &self.note {
            v.push(Seg::Text(t.clone(), accent()));
            return v;
        }
        let playing = self.media_now.as_ref().is_some_and(|m| m.playing);
        if self.media_now.is_some() && self.thumb.is_some() {
            v.push(Seg::Thumb);
        }
        if let Some(m) = self.media_now.as_ref().filter(|m| m.playing || self.hover_was) {
            v.push(Seg::Track(if m.artist.trim().is_empty() { m.title.clone() } else { format!("{} — {}", m.title, m.artist) }));
        }
        v.push(Seg::Text(chrono::Local::now().format("%H:%M").to_string(), FG));
        // напоминание скоро — обратный отсчёт с колокольчиком
        let stamp = chrono::Local::now().timestamp();
        if let Some(r) = self.cfg.reminders.iter().filter(|r| r.at > stamp && r.at - stamp <= 600).min_by_key(|r| r.at) {
            let mins = ((r.at - stamp) as f64 / 60.0).ceil() as i64;
            v.push(Seg::Text(format!("\u{E823} {mins} мин"), Color32::from_rgb(0xFF, 0x8A, 0x7A)));
        }
        if self.inbox_unread > 0 {
            let c = self.inbox.first().map_or([0x3A, 0x9B, 0xFF], |i| i.color);
            v.push(Seg::Badge(self.inbox_unread, Color32::from_rgb(c[0], c[1], c[2])));
        }
        if let (Some(end), true) = (self.timer_end, self.fx.is_none()) {
            v.push(Seg::Text(fmt_left(end), accent()));
        }
        if let (Some(since), true) = (self.call_since, self.fx.is_none()) {
            let secs = since.elapsed().as_secs();
            v.push(Seg::Text(format!("\u{E720} {:02}:{:02}", secs / 60, secs % 60), Color32::from_rgb(0x8E, 0x98, 0xFF)));
        }
        if let Some((pct, charging)) = battery_seg(self.stats.battery) {
            v.push(Seg::Battery(pct, charging));
        }
        if playing || self.sound.active() {
            v.push(Seg::Eq);
        } else if let Some(w) = &self.weather {
            let (icon, _) = weather::describe(w.code);
            v.push(Seg::Text(format!("{icon} {}", weather::deg(w.temp)), DIM));
        }
        v
    }

    fn pill_width(&self, ctx: &egui::Context, segs: &[Seg]) -> f32 {
        let mut w = 58.0;
        for s in segs {
            w += match s {
                Seg::Thumb => 20.0,
                Seg::Eq => 24.0,
                Seg::Track(t) => ctx.fonts(|f| f.layout_no_wrap(t.clone(), semi(13.0), FG).size().x).clamp(96.0, 170.0),
                Seg::Text(t, _) => ctx.fonts(|f| f.layout_no_wrap(t.clone(), semi(13.5), FG).size().x),
                Seg::Badge(n, _) => badge_w(ctx, *n),
                Seg::Battery(pct, _) => 25.0 + ctx.fonts(|f| f.layout_no_wrap(format!("{pct}%"), semi(12.5), FG).size().x),
            } + 10.0;
        }
        (w + 6.0).clamp(120.0, 520.0)
    }

    /// Персонаж в пилюле. Прорисовка глаз и персонажей перенесена из Atoll (Views/CharacterView.cs, MIT).
    fn draw_face(&self, p: &egui::Painter, o: Pos2, look: Vec2, now: Instant) {
        // все множители t в sin/cos должны быть кратны 0.1: фаза оборачивается по RIPPLE_PERIOD = 150π
        let t = self.ripple_phase / 7.5;
        let playing = self.media_now.as_ref().is_some_and(|m| m.playing);
        let emo = self.emotion;
        let music = playing && emo == Emotion::Normal;
        let look = vec2((look.x / 2.6).clamp(-1.0, 1.0), (look.y / 2.2).clamp(-1.0, 1.0));
        let blink = if now < self.blink_until { 1.0 } else { 0.0 };
        // общее движение тела: танец под музыку, дыхание, дрожь от злости, прыжки от радости
        let dance = if music { (t * 7.0).sin().abs() * 1.3 } else { 0.0 };
        let mut y = o.y + 9.0 - dance + (t * 2.2).sin() * 0.5;
        let mut cx = o.x + 18.0;
        match emo {
            Emotion::Angry => cx += (t * 55.0).sin() * 0.9,
            Emotion::Happy => y -= (t * 16.0).sin().abs() * 1.6,
            _ => {}
        }
        let white = Color32::WHITE;
        let ink = Color32::from_rgb(14, 14, 22);
        if self.flask_on() {
            // поплавок: свой персонаж острова — качается на поверхности жидкости и смотрит на курсор
            let rect = self.pill_rect;
            let x = rect.left() + 30.0;
            let (top, bottom, _) = self.surface_at(rect, rect.height() / 2.0, x);
            let (_, _, _) = (top, bottom, 0);
            let slope = self.surface_at(rect, rect.height() / 2.0, x + 5.0).0 - self.surface_at(rect, rect.height() / 2.0, x - 5.0).0;
            let hop = if emo == Emotion::Happy { (t * 16.0).sin().abs() * 3.0 } else { 0.0 };
            let y = (top - 2.0 - hop).clamp(rect.top() + 13.5, rect.bottom() - 8.0);
            let c = self.liquid_color();
            let lean = (slope * 0.35).clamp(-2.0, 2.0);
            // огонёк на макушке — в цвет жидкости
            p.line_segment([pos2(x + lean, y - 5.5), pos2(x + lean * 2.0, y - 8.6)], Stroke::new(1.2_f32, white));
            p.circle_filled(pos2(x + lean * 2.0, y - 9.4), 1.8, Color32::WHITE);
            p.circle_filled(pos2(x + lean * 2.0, y - 9.4), 1.1, Color32::from_rgb(c[0], c[1], c[2]));
            p.circle_filled(pos2(x, y), 6.8, white);
            p.add(egui::Shape::ellipse_filled(pos2(x - 2.6, y - 3.6), vec2(2.6, 1.3), rgba(255, 255, 255, 255)));
            // полоска по «ватерлинии»
            p.rect_filled(Rect::from_center_size(pos2(x, y + 3.4), vec2(11.8, 1.5)), Rounding::same(0.75), rgba(c[0], c[1], c[2], 230));
            let f = Face { p, emo, look, blink, mid: x };
            f.eye(x - 3.0, y - 0.6, 2.2, 2.7, ink, white, true);
            f.eye(x + 3.0, y - 0.6, 2.2, 2.7, ink, white, true);
            if emo == Emotion::Cool {
                for side in [-1.0, 1.0] {
                    p.rect(Rect::from_center_size(pos2(x + side * 3.2, y - 0.6), vec2(6.0, 5.0)), Rounding::same(2.0), Color32::from_rgb(18, 18, 26), Stroke::new(0.8_f32, ink));
                }
            }
            return;
        }
        let f = Face { p, emo, look, blink, mid: cx };
        match self.cfg.character {
            1 => {
                // робот: антенна с огоньком, тёмный экран со светящимися глазами, рот-эквалайзер
                p.line_segment([pos2(cx, y - 7.0), pos2(cx, y - 10.0)], Stroke::new(1.2_f32, white));
                let on = (t * if music { 12.0 } else { 3.0 }).sin() > 0.0 && emo != Emotion::Sleepy;
                p.circle_filled(pos2(cx, y - 10.5), 1.6, if on { accent() } else { white });
                p.rect_filled(Rect::from_min_size(pos2(cx - 10.0, y - 7.0), vec2(20.0, 15.0)), Rounding::same(4.5), white);
                p.rect_filled(Rect::from_min_size(pos2(cx - 8.0, y - 5.0), vec2(16.0, 8.0)), Rounding::same(2.5), ink);
                let eh = if emo == Emotion::Happy { 1.2 } else if emo == Emotion::Sleepy { 0.9 } else { 3.6 * (1.0 - blink) }.max(0.8);
                let (ox, oy) = (look.x * 1.3, look.y * 0.8);
                for x0 in [cx - 5.6, cx + 2.4] {
                    p.rect_filled(Rect::from_min_size(pos2(x0 + ox, y - 1.0 - eh / 2.0 + oy), vec2(3.2, eh)), Rounding::same(1.0), accent());
                }
                for i in 0..4 {
                    let mh = if music { 0.8 + (t * 9.0 + i as f32 * 1.3).sin().abs() * 2.2 } else { 0.9 };
                    p.rect_filled(Rect::from_min_size(pos2(cx - 4.4 + i as f32 * 2.4, y + 6.4 - mh), vec2(1.5, mh)), Rounding::ZERO, ink);
                }
            }
            2 => {
                // кот: уши иногда подёргиваются, розовый нос, усы
                let twitch = if (t * 1.3 + 1.0).sin() > 0.97 { 1.2 } else { 0.0 };
                p.add(egui::Shape::convex_polygon(vec![pos2(cx - 9.0, y - 2.0), pos2(cx - 8.5, y - 10.0 - twitch), pos2(cx - 2.5, y - 6.0)], white, Stroke::NONE));
                p.add(egui::Shape::convex_polygon(vec![pos2(cx + 9.0, y - 2.0), pos2(cx + 2.5, y - 6.0), pos2(cx + 8.5, y - 10.0)], white, Stroke::NONE));
                p.add(egui::Shape::ellipse_filled(pos2(cx, y + 1.0), vec2(10.0, 7.6), white));
                f.eye(cx - 4.2, y, 2.6, 3.0, ink, white, true);
                f.eye(cx + 4.2, y, 2.6, 3.0, ink, white, true);
                p.add(egui::Shape::ellipse_filled(pos2(cx, y + 3.4), vec2(1.1, 0.8), Color32::from_rgb(0xFF, 0x8F, 0xA3)));
                p.line_segment([pos2(cx - 9.5, y + 3.0), pos2(cx - 5.5, y + 3.6)], Stroke::new(0.6_f32, ink));
                p.line_segment([pos2(cx + 9.5, y + 3.0), pos2(cx + 5.5, y + 3.6)], Stroke::new(0.6_f32, ink));
            }
            3 => {
                // призрак: парит, подол колышется
                let y = y + (t * 2.4).sin() * 1.2;
                let (l, r, top, bottom) = (cx - 8.0, cx + 8.0, y - 9.0, y + 8.0);
                // тело собрано из простых фигур: купол, прямая часть и волнистый подол из кружков
                let body = rgba(255, 255, 255, 245);
                p.circle_filled(pos2(cx, top + 8.0), 8.0, body);
                p.rect_filled(Rect::from_min_max(pos2(l, top + 8.0), pos2(r, bottom - 2.0)), Rounding::ZERO, body);
                for i in 0..4 {
                    let x = l + 2.0 + i as f32 * 4.0;
                    let dip = (i as f32 * 1.6 + t * 6.0).sin() * 1.1;
                    p.circle_filled(pos2(x, bottom - 2.0 + dip * 0.5), 2.0, body);
                }
                f.eye(cx - 3.4, y - 2.0, 2.4, 3.1, ink, white, true);
                f.eye(cx + 3.4, y - 2.0, 2.4, 3.1, ink, white, true);
                if music {
                    p.add(egui::Shape::ellipse_filled(pos2(cx, y + 3.0), vec2(1.6, 1.0 + (t * 9.0).sin().abs() * 1.4), ink));
                }
            }
            4 => {
                // слайм: мягкая капля, которая слегка колышется
                let squash = (t * 3.0).sin() * 0.8;
                let (hw, hh, base) = (10.0 + squash * 0.5, 13.0 - squash, y + 8.0);
                let mut pts = Vec::new();
                for i in 0..=16 {
                    let a = std::f32::consts::PI * (1.0 + i as f32 / 16.0);
                    pts.push(pos2(cx + a.cos() * hw, base + a.sin() * hh));
                }
                p.add(egui::Shape::convex_polygon(pts, Color32::from_rgb(0x7C, 0xE8, 0x9A), Stroke::NONE));
                p.add(egui::Shape::ellipse_filled(pos2(cx - 3.5, base - hh * 0.72), vec2(2.4, 1.2), rgba(255, 255, 255, 120)));
                f.eye(cx - 3.8, base - hh * 0.42, 2.5, 2.9, ink, white, true);
                f.eye(cx + 3.8, base - hh * 0.42, 2.5, 2.9, ink, white, true);
            }
            5 => {
                // лисёнок: рыжая мордочка, большие уши с тёмными кончиками, белые щёки и чёрный нос
                let fox = Color32::from_rgb(0xFF, 0x9A, 0x4D);
                let tip = Color32::from_rgb(0x5A, 0x34, 0x2A);
                let perk = if (t * 1.1 + 2.0).sin() > 0.96 { 1.0 } else { 0.0 };
                for side in [-1.0f32, 1.0] {
                    let base_out = pos2(cx + side * 9.5, y - 1.0);
                    let base_in = pos2(cx + side * 2.5, y - 6.0);
                    let top = pos2(cx + side * 9.0, y - 12.0 - if side < 0.0 { perk } else { 0.0 });
                    p.add(egui::Shape::convex_polygon(vec![base_out, top, base_in], fox, Stroke::NONE));
                    // тёмный кончик уха
                    let k = 0.38;
                    let a = pos2(top.x + (base_out.x - top.x) * k, top.y + (base_out.y - top.y) * k);
                    let b = pos2(top.x + (base_in.x - top.x) * k, top.y + (base_in.y - top.y) * k);
                    p.add(egui::Shape::convex_polygon(vec![a, top, b], tip, Stroke::NONE));
                }
                // голова — широкий овал, книзу сужается в мордочку
                p.add(egui::Shape::ellipse_filled(pos2(cx, y + 0.5), vec2(10.5, 7.8), fox));
                p.add(egui::Shape::convex_polygon(vec![pos2(cx - 7.0, y + 2.0), pos2(cx + 7.0, y + 2.0), pos2(cx, y + 8.6)], fox, Stroke::NONE));
                // белые щёки
                p.add(egui::Shape::ellipse_filled(pos2(cx - 4.6, y + 4.0), vec2(4.2, 3.0), white));
                p.add(egui::Shape::ellipse_filled(pos2(cx + 4.6, y + 4.0), vec2(4.2, 3.0), white));
                p.add(egui::Shape::convex_polygon(vec![pos2(cx - 3.5, y + 4.5), pos2(cx + 3.5, y + 4.5), pos2(cx, y + 8.2)], white, Stroke::NONE));
                f.eye(cx - 4.0, y - 0.6, 2.3, 2.8, ink, white, true);
                f.eye(cx + 4.0, y - 0.6, 2.3, 2.8, ink, white, true);
                p.add(egui::Shape::ellipse_filled(pos2(cx, y + 6.6), vec2(1.5, 1.1), ink));
            }
            _ => {
                // глазки: два больших глаза с бликами в зрачках
                f.eye(cx - 6.4, y - 0.5, 5.9, 6.5, white, ink, false);
                f.eye(cx + 6.4, y - 0.5, 5.9, 6.5, white, ink, false);
            }
        }
        if emo == Emotion::Cool && self.cfg.character != 1 {
            // тёмные очки
            let (ex, ey) = if self.cfg.character == 0 { (6.4, y - 0.5) } else { (4.0, y - 0.5) };
            let size = if self.cfg.character == 0 { vec2(12.5, 9.0) } else { vec2(7.5, 6.0) };
            for side in [-1.0, 1.0] {
                p.rect(Rect::from_center_size(pos2(cx + side * ex, ey), size), Rounding::same(3.0), Color32::from_rgb(18, 18, 26), Stroke::new(1.0_f32, white));
            }
            p.line_segment([pos2(cx - ex + size.x / 2.0, ey - 1.5), pos2(cx + ex - size.x / 2.0, ey - 1.5)], Stroke::new(1.0_f32, white));
        }
        if emo == Emotion::Sleepy {
            p.text(pos2(o.x + 38.0, o.y + 2.0), Align2::CENTER_CENTER, "z", FontId::proportional(10.0), DIM);
        }
    }

    /// Идёт ли за окном дождь (1) или снег (2) — по коду погоды. 0 — сухо или эффект выключен.
    fn precipitation(&self) -> u8 {
        if test_mode() {
            match std::env::var("OSTROVOK_WX").as_deref() {
                Ok("rain") => return 1,
                Ok("snow") => return 2,
                _ => {}
            }
        }
        if self.cfg.no_wx {
            return 0;
        }
        match self.weather.as_ref().map(|w| w.code) {
            Some(51..=67 | 80..=82 | 95..=99) => 1,
            Some(71..=77 | 85 | 86) => 2,
            _ => 0,
        }
    }

    fn draw_pill(&self, ui: &egui::Ui, rect: Rect, segs: &[Seg], time: f64) {
        let p = ui.painter().with_clip_rect(rect);
        let cy = rect.center().y;
        // живая погода: за текстом пилюли тихо идёт дождь или снег
        let wet = self.precipitation();
        if wet > 0 {
            let (h, inset) = (rect.height(), rect.height() / 2.0);
            let span = (rect.width() - 2.0 * inset).max(1.0);
            let frac = |v: f32| v - v.floor();
            for i in 0..16 {
                let seed = i as f32 * 12.9898 + 4.1;
                let fx = frac(seed.sin() * 43758.547);
                let speed = if wet == 1 { 46.0 } else { 9.0 } * (0.7 + 0.6 * frac(seed * 1.7));
                let y = rect.top() - 5.0 + (time as f32 * speed + frac(seed * 3.1) * (h + 10.0)) % (h + 10.0);
                let fade = (1.0 - ((y - rect.center().y).abs() / (h / 2.0)).powi(4)).clamp(0.0, 1.0);
                if wet == 1 {
                    let x = rect.left() + inset + fx * span - (y - rect.top()) * 0.25;
                    p.line_segment([pos2(x, y), pos2(x - 1.4, y + 5.5)], Stroke::new(1.1_f32, rgba(150, 195, 255, (70.0 * fade) as u8)));
                } else {
                    let x = rect.left() + inset + fx * span + (time as f32 * 0.9 + seed).sin() * 3.0;
                    p.circle_filled(pos2(x, y), 1.0 + 0.6 * frac(seed * 5.3), rgba(255, 255, 255, (100.0 * fade) as u8));
                }
            }
        }
        let mut x = rect.left() + 58.0;
        for s in segs {
            match s {
                Seg::Thumb => {
                    if let Some(t) = &self.thumb {
                        rounded_image(&p, t, Rect::from_min_size(pos2(x, cy - 10.0), vec2(20.0, 20.0)), 5.0);
                    }
                    x += 30.0;
                }
                Seg::Eq => {
                    let color = self.album.map_or(accent(), |c| Color32::from_rgb(c[0], c[1], c[2]));
                    let live = self.sound.active();
                    for i in 0..5 {
                        // пять столбиков: каждый — среднее по трём соседним полосам настоящего звука
                        let h = if live {
                            let k = i * 3;
                            3.0 + 13.0 * ((self.bars[k] + self.bars[k + 1] + self.bars[k + 2]) / 3.0).powf(1.4)
                        } else {
                            4.0 + 9.0 * (0.5 + 0.5 * ((time * (5.0 + i as f64 * 1.7) + i as f64 * 1.3).sin() as f32))
                        };
                        let bx = x + i as f32 * 4.5;
                        p.rect_filled(Rect::from_min_max(pos2(bx, cy + 8.0 - h), pos2(bx + 3.0, cy + 8.0)), Rounding::same(1.5), color);
                    }
                    x += 34.0;
                }
                Seg::Track(t) => {
                    // название трека; под курсором на его месте — кнопки управления
                    let full = p.layout_no_wrap(t.clone(), semi(13.0), FG);
                    let w = full.size().x.clamp(96.0, 170.0);
                    let area = Rect::from_min_size(pos2(x, rect.top()), vec2(w, rect.height()));
                    if self.hover_was {
                        let playing = self.media_now.as_ref().is_some_and(|m| m.playing);
                        for (k, glyph) in ["\u{E892}", if playing { "\u{E769}" } else { "\u{E768}" }, "\u{E893}"].into_iter().enumerate() {
                            let hit = Rect::from_center_size(pos2(area.center().x + (k as f32 - 1.0) * 30.0, cy), vec2(28.0, 26.0));
                            let resp = ui.interact(hit, Id::new(("pill-media", k)), Sense::click());
                            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
                            if hov > 0.0 {
                                p.circle_filled(hit.center(), 12.0, rgba(255, 255, 255, (36.0 * hov) as u8));
                            }
                            p.text(hit.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(if k == 1 { 15.0 } else { 12.5 }), FG);
                            if resp.clicked() {
                                let _ = self.media_tx.send(match k {
                                    0 => media::Cmd::Prev,
                                    1 => media::Cmd::Toggle,
                                    _ => media::Cmd::Next,
                                });
                            }
                        }
                    } else {
                        // длинное название плавно ездит туда-обратно с паузами по краям
                        let over = full.size().x - w;
                        let shift = if over > 0.0 {
                            let travel = over / 22.0;
                            let period = travel + 3.0;
                            let ph = time as f32 % (2.0 * period);
                            let u = if ph < period { ph } else { 2.0 * period - ph };
                            (u - 1.5).clamp(0.0, travel) / travel * over
                        } else {
                            0.0
                        };
                        let h = full.size().y;
                        p.with_clip_rect(area.intersect(rect)).galley(pos2(x - shift, cy - h / 2.0), full, FG);
                    }
                    x += w + 10.0;
                }
                Seg::Text(t, c) => {
                    let r = p.text(pos2(x, cy), Align2::LEFT_CENTER, t, semi(13.5), *c);
                    x += r.width() + 10.0;
                }
                Seg::Badge(n, c) => {
                    let w = badge_w(ui.ctx(), *n);
                    let r = Rect::from_min_size(pos2(x, cy - 9.0), vec2(w, 18.0));
                    // неподвижное мягкое свечение: анимация заставляла бы остров перерисовываться без остановки
                    let glow = 0.6;
                    p.rect_filled(r.expand(2.0 + glow), Rounding::same(11.0 + glow), Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (26.0 + 30.0 * glow) as u8));
                    p.rect_filled(r, Rounding::same(9.0), mix(*c, Color32::WHITE, 0.08));
                    p.rect_filled(Rect::from_min_max(r.min + vec2(4.0, 1.5), pos2(r.right() - 4.0, r.top() + 6.5)), Rounding::same(4.0), rgba(255, 255, 255, 34));
                    p.text(r.center() + vec2(0.0, 0.5), Align2::CENTER_CENTER, badge_text(*n), semi(11.5), Color32::WHITE);
                    x += w + 10.0;
                }
                Seg::Battery(pct, charging) => {
                    // корпус, носик и заливка по заряду; при зарядке — молния поверх. Всё неподвижно
                    let [r0, g0, b0] = battery_rgb(*pct, *charging);
                    let col = Color32::from_rgb(r0, g0, b0);
                    let body = Rect::from_min_size(pos2(x, cy - 5.5), vec2(19.0, 11.0));
                    p.rect_stroke(body, Rounding::same(3.0), Stroke::new(1.3_f32, mix(col, Color32::WHITE, 0.35)));
                    p.rect_filled(Rect::from_min_size(pos2(body.right() + 1.0, cy - 2.0), vec2(2.0, 4.0)), Rounding::same(1.0), mix(col, Color32::WHITE, 0.35));
                    let inner = body.shrink(2.2);
                    let fill = (inner.width() * (*pct as f32 / 100.0)).max(2.0);
                    p.rect_filled(Rect::from_min_size(inner.min, vec2(fill, inner.height())), Rounding::same(1.5), col);
                    if *charging {
                        p.text(body.center(), Align2::CENTER_CENTER, "\u{E945}", FontId::proportional(10.0), Color32::WHITE);
                    }
                    let r = p.text(pos2(x + 25.0, cy), Align2::LEFT_CENTER, format!("{pct}%"), semi(12.5), col);
                    x += 25.0 + r.width() + 10.0;
                }
            }
        }
        // ход трека — тонкая линия по нижней кромке пилюли
        if let Some(m) = self.media_now.as_ref().filter(|m| m.playing && m.dur > 0.0) {
            let frac = media::track_frac(m.pos, m.at.elapsed().as_secs_f64(), m.dur, true);
            let color = self.album.map_or(accent(), |c| Color32::from_rgb(c[0], c[1], c[2]));
            let (l, r, y) = (rect.left() + 18.0, rect.right() - 18.0, rect.bottom() - 3.5);
            p.line_segment([pos2(l, y), pos2(r, y)], Stroke::new(1.5_f32, rgba(255, 255, 255, 26)));
            p.line_segment([pos2(l, y), pos2(l + (r - l) * frac, y)], Stroke::new(1.5_f32, color));
        }
    }

    fn ui_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        // одна строка: слева значки разделов, рядом текстом — страницы выбранного раздела
        const GROUPS: [(&str, &str, &[(Tab, &str)]); 6] = [
            ("\u{E80F}", "Обзор", &[(Tab::Home, "Сводка"), (Tab::Music, "Музыка"), (Tab::Weather, "Погода"), (Tab::Calendar, "Календарь")]),
            ("\u{E7F4}", "Система", &[(Tab::Control, "Управление"), (Tab::Sound, "Звук"), (Tab::System, "Монитор")]),
            ("\u{E90F}", "Инструменты", &[(Tab::Timer, "Таймер"), (Tab::Remind, "Напоминания"), (Tab::Qr, "QR-код")]),
            ("\u{E70B}", "Записи", &[(Tab::Clip, "Буфер"), (Tab::Inbox, "Уведомления"), (Tab::Todo, "Дела"), (Tab::Notes, "Заметки"), (Tab::Shelf, "Полка")]),
            ("\u{E8BD}", "ИИ", &[(Tab::Ai, "ИИ"), (Tab::Jarvis, "Джарвис")]),
            ("\u{E713}", "Настройки", &[(Tab::Settings, "Настройки")]),
        ];
        let current = GROUPS.iter().position(|g| g.2.iter().any(|t| t.0 == self.tab)).unwrap_or(0);
        let mut go: Option<Tab> = None;
        let row = ui.available_rect_before_wrap();
        let mid = row.top() + 15.0;
        let mut x = row.left() + 46.0;
        for (gi, (glyph, name, pages)) in GROUPS.iter().enumerate() {
            let id = ui.id().with(("group", *name));
            let r = Rect::from_min_size(pos2(x, row.top()), vec2(34.0, 30.0));
            let resp = ui.interact(r, id, Sense::click()).on_hover_text(*name);
            let sel = ui.ctx().animate_bool_with_time(id.with("sel"), gi == current, 0.14);
            let hov = ui.ctx().animate_bool_with_time(id.with("hov"), resp.hovered(), 0.10);
            if sel > 0.0 || hov > 0.0 {
                glass_chip(ui.painter(), r, 11.0, sel * 0.9 + hov * (1.0 - sel) * 0.4);
            }
            ui.painter().text(r.center(), Align2::CENTER_CENTER, *glyph, FontId::proportional(15.0), mix(DIM, FG, sel.max(hov * 0.6)));
            if resp.clicked() && gi != current {
                go = Some(pages[0].0);
            }
            x += 37.0;
        }
        x += 5.0;
        ui.painter().line_segment([pos2(x, row.top() + 8.0), pos2(x, row.top() + 22.0)], Stroke::new(1.0_f32, rgba(255, 255, 255, 34)));
        x += 11.0;
        let pages = GROUPS[current].2;
        if pages.len() > 1 {
            for (t, name) in pages {
                let id = ui.id().with(("page", *name));
                let width = ui.painter().layout_no_wrap(name.to_string(), semi(13.0), FG).size().x + 18.0;
                let r = Rect::from_min_size(pos2(x, row.top() + 2.0), vec2(width, 26.0));
                let resp = ui.interact(r, id, Sense::click());
                let sel = ui.ctx().animate_bool_with_time(id.with("sel"), self.tab == *t, 0.14);
                let hov = ui.ctx().animate_bool_with_time(id.with("hov"), resp.hovered(), 0.10);
                ui.painter().text(r.center(), Align2::CENTER_CENTER, *name, semi(13.0), mix(DIM, FG, sel.max(hov * 0.6)));
                if resp.clicked() {
                    go = Some(*t);
                }
                x += width;
            }
        } else {
            ui.painter().text(pos2(x + 2.0, mid), Align2::LEFT_CENTER, GROUPS[current].1, semi(13.5), FG);
        }
        {
            let r = Rect::from_min_size(pos2(row.right() - 34.0, row.top()), vec2(34.0, 30.0));
            let resp = ui.interact(r, ui.id().with("search"), Sense::click()).on_hover_text(format!("Поиск · {}", self.hotkey_name));
            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
            glass_chip(ui.painter(), r, 11.0, 0.35 + 0.5 * hov);
            ui.painter().text(r.center(), Align2::CENTER_CENTER, "\u{E721}", FontId::proportional(14.0), mix(DIM, FG, hov));
            if resp.clicked() {
                self.toggle_palette(ctx);
            }
        }
        ui.allocate_space(vec2(ui.available_width(), 30.0));
        if let Some(t) = go {
            self.tab = t;
        }
        ui.add_space(10.0);
        if self.tab != self.tab_seen {
            self.tab_dir = if (self.tab as u8) > (self.tab_seen as u8) { 1.0 } else { -1.0 };
            self.tab_seen = self.tab;
            self.tab_at = Instant::now();
        }
        let tab_t = ease(self.tab_at.elapsed().as_secs_f32() / 0.16);
        let area = ui.available_rect_before_wrap().translate(vec2((1.0 - tab_t) * 8.0 * self.tab_dir, 0.0));
        let mut content = ui.new_child(egui::UiBuilder::new().max_rect(area));
        content.multiply_opacity(tab_t);
        let ui = &mut content;
        match self.tab {
            Tab::Home => self.ui_home(ui, ctx),
            Tab::Control => self.ui_control(ui),
            Tab::Music => self.ui_music(ui),
            Tab::Remind => self.ui_remind(ui),
            Tab::Qr => self.ui_qr(ui),
            Tab::Shelf => self.ui_shelf(ui),
            Tab::Ai => self.ui_ai(ui),
            Tab::Jarvis => self.ui_jarvis(ui),
            Tab::System => self.ui_system(ui),
            Tab::Sound => self.ui_sound(ui),
            Tab::Weather => self.ui_weather(ui),
            Tab::Timer => {
                self.ui_timer(ui);
                self.ui_focus_week(ui);
            }
            Tab::Calendar => {
                self.ui_calendar(ui);
                self.ui_world(ui);
            }
            Tab::Clip => self.ui_clip(ui),
            Tab::Inbox => self.ui_inbox(ui),
            Tab::Notes => self.ui_notes(ui),
            Tab::Todo => self.ui_todo(ui),
            Tab::Shot => self.ui_shot(ui),
            Tab::Settings => self.ui_settings(ui, ctx),
        }
    }

    /// Инструменты → QR-код.
    fn ui_qr(&mut self, ui: &mut egui::Ui) {
        // переключатель «Текст / Wi‑Fi» — две капсулы
        ui.horizontal(|ui| {
            for (wifi, label) in [(false, "\u{E8C1}  Текст"), (true, "\u{E701}  Wi‑Fi")] {
                let w = ui.painter().layout_no_wrap(label.to_string(), semi(12.5), FG).size().x + 26.0;
                let (r, resp) = ui.allocate_exact_size(vec2(w, 28.0), Sense::click());
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
                let on = self.qr_wifi == wifi;
                glass_chip(ui.painter(), r, 14.0, if on { 1.1 } else { 0.4 + 0.4 * hov });
                ui.painter().text(r.center(), Align2::CENTER_CENTER, label, semi(12.5), if on { accent() } else { mix(DIM, FG, hov) });
                if resp.clicked() {
                    self.qr_wifi = wifi;
                }
            }
        });
        ui.add_space(4.0);
        let wifi_text;
        let text: &str = if self.qr_wifi {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.wifi_ssid).char_limit(1000).margin(egui::Margin::symmetric(9.0, 7.0)).hint_text(RichText::new("Сеть").color(DIM)).desired_width(150.0));
                if self.wifi_secure {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.wifi_pass)
                            .char_limit(1000)
                            .password(!self.wifi_show)
                            .margin(egui::Margin::symmetric(9.0, 7.0))
                            .hint_text(RichText::new("Пароль").color(DIM))
                            .desired_width(150.0),
                    );
                    if jelly(ui, if self.wifi_show { "\u{ED1A}" } else { "\u{E7B3}" }).on_hover_text(if self.wifi_show { "Скрыть пароль" } else { "Показать пароль" }).clicked() {
                        self.wifi_show = !self.wifi_show;
                    }
                }
                if jelly(ui, if self.wifi_secure { "WPA" } else { "Без пароля" }).on_hover_text("Тип защиты сети — щелчок меняет").clicked() {
                    self.wifi_secure = !self.wifi_secure;
                }
            });
            wifi_text = wifi_payload(self.wifi_ssid.trim(), &self.wifi_pass, self.wifi_secure);
            &wifi_text
        } else {
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 120.0).max(120.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.qr_text)
                        .char_limit(1000)
                        .margin(egui::Margin::symmetric(9.0, 7.0))
                        .hint_text(RichText::new("Текст или ссылка").color(DIM))
                        .desired_width(w),
                );
                if jelly(ui, "Из буфера").clicked() {
                    self.qr_text = self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()).unwrap_or_default().chars().take(400).collect();
                }
            });
            self.qr_text.trim()
        };
        let text = text.to_string();
        let text = text.as_str();
        let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), 196.0), Sense::hover());
        if text.is_empty() {
            let hint = if self.qr_wifi { "Впишите имя сети — телефон подключится, наведя камеру" } else { "Впишите текст или ссылку — здесь появится код" };
            empty_at(ui.painter(), area.center(), "\u{ED14}", "Кода пока нет", hint);
            return;
        }
        // кодируем только при смене текста; копия клеток дешевле кодирования
        match qr_cached(&mut self.qr_cache, text).cloned() {
            Some((n, colors)) => {
                let side = 168.0;
                let sq = Rect::from_center_size(area.center() - vec2(90.0, 0.0), vec2(side, side));
                let p = ui.painter();
                p.rect_filled(sq.expand(12.0), Rounding::same(16.0), Color32::WHITE);
                let cell = side / n as f32;
                let ink = mix(Color32::from_rgb(18, 16, 34), accent(), 0.18);
                // «глазки» по углам рисуются отдельно — скруглёнными квадратами
                let finder = |x: usize, y: usize| (x < 7 && y < 7) || (x >= n - 7 && y < 7) || (x < 7 && y >= n - 7);
                for (i, c) in colors.iter().enumerate() {
                    let (x, y) = (i % n, i / n);
                    if *c && !finder(x, y) {
                        let at = sq.min + vec2((x as f32 + 0.5) * cell, (y as f32 + 0.5) * cell);
                        p.circle_filled(at, cell * 0.49, ink);
                    }
                }
                for (fx, fy) in [(0usize, 0usize), (n - 7, 0), (0, n - 7)] {
                    let o = sq.min + vec2(fx as f32 * cell, fy as f32 * cell);
                    let outer = Rect::from_min_size(o, vec2(7.0 * cell, 7.0 * cell));
                    p.rect_stroke(outer.shrink(cell / 2.0), Rounding::same(cell * 1.8), Stroke::new(cell, ink));
                    p.rect_filled(outer.shrink(cell * 2.0), Rounding::same(cell * 1.1), ink);
                }
                // справа — что в коде и что с ним сделать
                let side_r = Rect::from_min_max(pos2(sq.right() + 40.0, sq.top() + 6.0), pos2(area.right(), sq.bottom()));
                let kind = qr_kind(text);
                p.text(side_r.min, Align2::LEFT_TOP, kind, semi(14.0), FG);
                // у Wi‑Fi показываем только сеть и защиту — пароль на экран не выводим
                let preview: String = if self.qr_wifi {
                    format!("Сеть «{}» · {}", self.wifi_ssid.trim().chars().take(32).collect::<String>(), if self.wifi_secure { "WPA" } else { "без пароля" })
                } else {
                    text.chars().take(60).collect::<String>() + if text.chars().count() > 60 { "…" } else { "" }
                };
                p.with_clip_rect(side_r).text(side_r.min + vec2(0.0, 22.0), Align2::LEFT_TOP, preview, FontId::proportional(12.0), DIM);
                p.text(side_r.min + vec2(0.0, 42.0), Align2::LEFT_TOP, format!("{n}×{n} точек · наводите камеру телефона"), FontId::proportional(11.5), DIM);
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(side_r.min + vec2(0.0, 74.0), side_r.max)));
                let (mut copy, mut save) = (false, false);
                child.horizontal(|ui| {
                    copy = jelly(ui, "Копировать картинку").clicked();
                    save = jelly(ui, "Сохранить PNG").clicked();
                });
                if copy || save {
                    // для файла и буфера — строгие квадраты: так код читают любые сканеры
                    let (scale, quiet) = (10u32, 4u32);
                    let size = (n as u32 + quiet * 2) * scale;
                    let img = image::RgbaImage::from_fn(size, size, |px, py| {
                        let (x, y) = ((px / scale) as i64 - quiet as i64, (py / scale) as i64 - quiet as i64);
                        let dark = x >= 0 && y >= 0 && (x as usize) < n && (y as usize) < n && colors[y as usize * n + x as usize];
                        if dark { image::Rgba([0, 0, 0, 255]) } else { image::Rgba([255, 255, 255, 255]) }
                    });
                    if save {
                        let dir = sys::pictures_dir();
                        let _ = std::fs::create_dir_all(&dir);
                        let path = dir.join(format!("QR {}.png", chrono::Local::now().format("%Y-%m-%d %H-%M-%S")));
                        match img.save(&path) {
                            Ok(_) => {
                                self.notify("Сохранено в «Изображения»");
                                sys::reveal(&path);
                            }
                            Err(_) => self.notify("Не удалось сохранить"),
                        }
                    }
                    if copy {
                        // свою запись в буфер не принимаем за новый снимок
                        self.clip_skip = true;
                        let ok = self.clipboard.as_mut().is_some_and(|cb| {
                            cb.set_image(arboard::ImageData { width: size as usize, height: size as usize, bytes: std::borrow::Cow::Owned(img.into_raw()) }).is_ok()
                        });
                        self.notify(if ok { "Картинка кода скопирована" } else { "Не удалось скопировать" });
                    }
                }
            }
            None => {
                ui.painter().text(area.center(), Align2::CENTER_CENTER, "Текст слишком длинный для кода", FontId::proportional(13.5), DIM);
            }
        }
    }

    /// Инструменты → Напоминания.
    fn ui_remind(&mut self, ui: &mut egui::Ui) {
        let mut add = false;
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 250.0).max(120.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.remind_text)
                    .margin(egui::Margin::symmetric(9.0, 7.0))
                    .hint_text(RichText::new("О чём напомнить").color(DIM))
                    .desired_width(w),
            );
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.remind_time)
                    .margin(egui::Margin::symmetric(9.0, 7.0))
                    .hint_text(RichText::new("18:30 или +15").color(DIM))
                    .desired_width(110.0),
            );
            add = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            add |= jelly(ui, "Добавить").clicked();
        });
        // быстрое время: щелчок подставляет время, а если текст уже вписан — сразу добавляет
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            use chrono::Timelike;
            let now = chrono::Local::now();
            let to_tomorrow_9 = (24 * 60 - (now.hour() * 60 + now.minute()) as i64 + 9 * 60).max(1);
            for (label, spec) in [("+5 мин", "+5".to_string()), ("+15 мин", "+15".into()), ("+1 час", "+60".into()), ("вечером 19:00", "19:00".into()), ("завтра 9:00", format!("+{to_tomorrow_9}"))] {
                let w = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(12.0), FG).size().x + 20.0;
                let (r, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
                let on = self.remind_time == spec;
                glass_chip(ui.painter(), r, 13.0, if on { 1.1 } else { 0.4 + 0.5 * hov });
                ui.painter().text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(12.0), if on { accent() } else { mix(DIM, FG, hov) });
                if resp.clicked() {
                    self.remind_time = spec;
                    if !self.remind_text.trim().is_empty() {
                        add = true;
                    }
                }
            }
            // повтор: щелчок перебирает «без повтора → каждый день → по будням → каждую неделю»
            let on = self.remind_repeat != 0;
            let label = format!("\u{E8EE}  {}", repeat_name(self.remind_repeat));
            let w = ui.painter().layout_no_wrap(label.clone(), FontId::proportional(12.0), FG).size().x + 20.0;
            let (r, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
            let resp = resp.on_hover_text("Повтор нового напоминания — щелчок меняет");
            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
            glass_chip(ui.painter(), r, 13.0, if on { 1.1 } else { 0.4 + 0.5 * hov });
            ui.painter().text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(12.0), if on { accent() } else { mix(DIM, FG, hov) });
            if resp.clicked() {
                self.remind_repeat = (self.remind_repeat + 1) % 4;
            }
        });
        ui.add_space(2.0);
        ui.label(RichText::new("Можно и из поиска: «напомни 18:30 позвонить» или «напомни через 15 мин чай».").color(DIM).size(11.5));
        if add {
            self.add_reminder();
        }
        let mut remove = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if self.cfg.reminders.is_empty() {
                empty_state(ui, "\u{E823}", "Напоминаний пока нет", "Впишите дело и время выше — остров напомнит");
            }
            for (i, r) in self.cfg.reminders.iter().enumerate() {
                let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
                glass_chip(ui.painter(), row, 10.0, 0.45);
                let when = chrono::DateTime::from_timestamp(r.at, 0)
                    .map(|d| d.with_timezone(&chrono::Local).format("%d.%m  %H:%M").to_string())
                    .unwrap_or_default();
                let p = ui.painter().with_clip_rect(row);
                p.text(pos2(row.left() + 14.0, row.center().y), Align2::LEFT_CENTER, when, semi(13.0), accent());
                let mut text_end = row.right() - 40.0;
                if r.repeat != 0 {
                    let g = p.layout_no_wrap(format!("\u{E8EE} {}", repeat_name(r.repeat)), FontId::proportional(11.5), accent());
                    let at = pos2(text_end - g.size().x, row.center().y - g.size().y / 2.0);
                    text_end = at.x - 10.0;
                    p.galley(at, g, accent());
                }
                p.with_clip_rect(Rect::from_min_max(pos2(row.left() + 120.0, row.top()), pos2(text_end, row.bottom())))
                    .text(pos2(row.left() + 120.0, row.center().y), Align2::LEFT_CENTER, &r.text, FontId::proportional(13.5), FG);
                let x = Rect::from_center_size(pos2(row.right() - 20.0, row.center().y), vec2(26.0, 26.0));
                let resp = ui.interact(x, ui.id().with(("del-reminder", i)), Sense::click());
                ui.painter().text(x.center(), Align2::CENTER_CENTER, "\u{E711}", FontId::proportional(12.0), if resp.hovered() { FG } else { DIM });
                if resp.clicked() {
                    remove = Some(i);
                }
            }
        });
        if let Some(i) = remove {
            if i < self.cfg.reminders.len() {
                self.cfg.reminders.remove(i);
                self.cfg.save();
            }
        }
    }

    fn add_reminder(&mut self) {
        use chrono::{Local, NaiveTime};
        let text = self.remind_text.trim().to_string();
        let spec = self.remind_time.trim().replace(',', ".");
        if text.is_empty() {
            self.notify("Впишите, о чём напомнить");
            return;
        }
        let now = Local::now();
        let at = if let Ok(time) = NaiveTime::parse_from_str(&spec, "%H:%M") {
            let today = now.date_naive().and_time(time);
            let day = if today <= now.naive_local() { today + chrono::Duration::days(1) } else { today };
            resolve_here(day).map(|d| d.timestamp())
        } else {
            spec.trim_start_matches('+').parse::<f64>().ok().filter(|m| *m > 0.0 && *m < 60.0 * 24.0 * 30.0).map(|m| now.timestamp() + (m * 60.0) as i64)
        };
        match at {
            Some(at) => {
                self.cfg.reminders.push(store::Reminder { text, at, repeat: self.remind_repeat });
                self.cfg.reminders.sort_by_key(|r| r.at);
                self.cfg.save();
                self.remind_text.clear();
                self.remind_time.clear();
                self.notify("Напоминание добавлено");
            }
            None => self.notify("Не понял время: напишите 18:30 или +15"),
        }
    }

    /// Записи → Полка: файлы значками, как на рабочем столе. Щелчок открывает, правый щелчок — папку файла.
    /// Записи → Дела: короткий список с галочками. Сделанное зачёркивается и уходит вниз.
    fn ui_todo(&mut self, ui: &mut egui::Ui) {
        let mut add = false;
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.todo_edit)
                    .margin(egui::Margin::symmetric(9.0, 7.0))
                    .hint_text(RichText::new("Что сделать · «до пятницы», «завтра», «до 5.12»").color(DIM))
                    .desired_width(ui.available_width() - 112.0),
            );
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            add = jelly(ui, "Добавить").clicked() || enter;
            if enter {
                r.request_focus();
            }
        });
        if add {
            let text = self.todo_edit.trim().to_string();
            if !text.is_empty() {
                self.cfg.todos.insert(0, new_todo(&text));
                self.cfg.todos.truncate(60);
                self.todo_edit.clear();
                self.cfg.save();
            }
        }
        if self.cfg.todos.is_empty() {
            empty_state(ui, "\u{E73E}", "Дел нет", "Впишите дело выше и нажмите Enter");
            return;
        }
        let left = self.cfg.todos.iter().filter(|t| !t.done).count();
        let done = self.cfg.todos.len() - left;
        let mut clear_done = false;
        // строка прогресса: «Сделано 1 из 4», полоска и тихая ссылка «убрать сделанные»
        {
            ui.add_space(4.0);
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
            let p = ui.painter().clone();
            let total = self.cfg.todos.len();
            let title = if left == 0 { "Всё сделано".to_string() } else { format!("Сделано {done} из {total}") };
            let t = p.text(pos2(row.left() + 4.0, row.center().y), Align2::LEFT_CENTER, title, semi(13.5), if left == 0 { accent() } else { FG });
            let link_w = if done > 0 { 132.0 } else { 0.0 };
            if done > 0 {
                let lr = Rect::from_min_max(pos2(row.right() - link_w, row.top()), row.max);
                let resp = ui.interact(lr, ui.id().with("todo-clear"), Sense::click()).on_hover_text("Удалить отмеченные дела");
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
                let g = p.text(pos2(row.right() - 4.0, row.center().y), Align2::RIGHT_CENTER, "убрать сделанные", FontId::proportional(12.5), mix(DIM, FG, hov));
                if hov > 0.0 {
                    p.line_segment([pos2(g.left(), g.bottom()), pos2(g.right(), g.bottom())], Stroke::new(1.0_f32, rgba(255, 255, 255, (90.0 * hov) as u8)));
                }
                clear_done = resp.clicked();
            }
            let bar = Rect::from_min_max(pos2(t.right() + 14.0, row.center().y - 2.5), pos2(row.right() - link_w - 14.0, row.center().y + 2.5));
            if bar.width() > 20.0 {
                let frac = ui.ctx().animate_value_with_time(ui.id().with("todo-frac"), done as f32 / total.max(1) as f32, 0.35);
                p.rect_filled(bar, Rounding::same(2.5), rgba(255, 255, 255, 22));
                if frac > 0.0 {
                    let fill = Rect::from_min_max(bar.min, pos2(bar.left() + (bar.width() * frac).max(5.0), bar.bottom()));
                    p.rect_filled(fill, Rounding::same(2.5), accent());
                }
            }
            ui.add_space(6.0);
        }
        let (mut toggle, mut remove) = (None, None);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            // сначала несделанные со сроком (ближайший выше), потом без срока, потом сделанные
            let today = chrono::Local::now().date_naive();
            let due_of = |t: &store::Todo| t.due.as_deref().and_then(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").ok());
            let mut order: Vec<usize> = (0..self.cfg.todos.len()).collect();
            order.sort_by_key(|i| {
                let t = &self.cfg.todos[*i];
                (t.done, t.done || due_of(t).is_none(), if t.done { None } else { due_of(t) })
            });
            for i in order {
                let item = &self.cfg.todos[i];
                // у сделанного дела «просрочено» бессмысленно — показываем просто дату
                let due = due_of(item).map(|d| if item.done { due_date_label(d) } else { due_label(d, today) });
                let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width() - 6.0, 38.0), Sense::click());
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
                let k = ui.ctx().animate_bool_with_time(ui.id().with(("todo-done", &item.text)), item.done, 0.16);
                let p = ui.painter();
                // сделанные карточки тише: меньше стекла
                glass_chip(p, row, 12.0, (0.42 - 0.22 * k) + 0.45 * hov);
                let c = pos2(row.left() + 20.0, row.center().y);
                let a = accent();
                p.circle_stroke(c, 8.5, Stroke::new(1.5_f32, mix(DIM, a, k.max(hov * 0.6))));
                if k > 0.0 {
                    p.circle_filled(c, 8.5 * k, a);
                    p.text(c, Align2::CENTER_CENTER, "\u{E73E}", FontId::proportional(9.5), Color32::from_rgba_unmultiplied(16, 14, 28, (k * 255.0) as u8));
                }
                // срок — капсула справа: просрочено красным, сегодня оранжевым, остальное тихо
                let mut text_end = row.right() - 34.0;
                if let Some((label, level)) = &due {
                    let col = if item.done { DIM } else {
                        match level { 2 => Color32::from_rgb(0xFF, 0x6B, 0x6B), 1 => Color32::from_rgb(0xFF, 0xB3, 0x40), _ => mix(DIM, FG, 0.35) }
                    };
                    let g = p.layout_no_wrap(label.clone(), FontId::proportional(11.5), col);
                    let cap = Rect::from_min_size(pos2(text_end - g.size().x - 18.0, row.center().y - 10.0), vec2(g.size().x + 18.0, 20.0));
                    let a = if *level > 0 && !item.done { 40 } else { 16 };
                    p.rect_filled(cap, Rounding::same(10.0), Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), a));
                    p.galley(cap.center() - g.size() / 2.0, g, col);
                    text_end = cap.left() - 8.0;
                }
                let text = p
                    .with_clip_rect(Rect::from_min_max(pos2(row.left() + 40.0, row.top()), pos2(text_end, row.bottom())))
                    .text(pos2(row.left() + 40.0, row.center().y), Align2::LEFT_CENTER, &item.text, FontId::proportional(13.5), mix(FG, DIM, k));
                if k > 0.0 {
                    let end = text.left() + text.width().min(text_end - text.left()) * k;
                    p.line_segment([pos2(text.left(), row.center().y + 1.0), pos2(end, row.center().y + 1.0)], Stroke::new(1.2_f32, DIM));
                }
                let x = Rect::from_center_size(pos2(row.right() - 16.0, row.center().y), vec2(24.0, 24.0));
                let del = ui.interact(x, ui.id().with(("todo-del", i)), Sense::click());
                if hov > 0.0 || del.hovered() {
                    ui.painter().text(x.center(), Align2::CENTER_CENTER, "\u{E711}", FontId::proportional(11.0), if del.hovered() { FG } else { DIM });
                }
                if del.clicked() {
                    remove = Some(i);
                } else if resp.clicked() {
                    toggle = Some(i);
                }
            }
        });
        if let Some(i) = toggle {
            self.cfg.todos[i].done = !self.cfg.todos[i].done;
            self.cfg.save();
        }
        if let Some(i) = remove {
            self.cfg.todos.remove(i);
            self.cfg.save();
        }
        if clear_done {
            self.cfg.todos.retain(|t| !t.done);
            self.cfg.save();
        }
    }

    fn ui_shelf(&mut self, ui: &mut egui::Ui) {
        if self.cfg.shelf.is_empty() {
            empty_state(ui, "\u{E8B7}", "Полка пуста", "Перетащите файл на остров — он останется под рукой");
            return;
        }
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 120.0).max(80.0);
            ui.allocate_ui_with_layout(vec2(w, 30.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(egui::Label::new(RichText::new("Щелчок — открыть, правый — показать в папке, можно вытащить мышью").color(DIM).size(12.5)).truncate());
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if jelly(ui, "Очистить").clicked() {
                    self.cfg.shelf.clear();
                    self.shelf_meta.clear();
                    self.shelf_thumbs.clear();
                    self.cfg.save();
                }
            });
        });
        // забираем миниатюры, прочитанные в фоне
        for (path, px) in std::mem::take(&mut *lock(&self.shelf_thumb_q)) {
            if !self.cfg.shelf.contains(&path) {
                continue;
            }
            let tex = px.map(|(w, h, px)| ui.ctx().load_texture(format!("shelf-thumb-{path}"), egui::ColorImage::from_rgba_unmultiplied([w, h], &px), egui::TextureOptions::LINEAR));
            self.shelf_thumbs.insert(path, tex);
        }
        let mut drag: Option<String> = None;
        let (mut open, mut remove) = (None, None);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                for (i, path) in self.cfg.shelf.iter().enumerate() {
                    let (cell, resp) = ui.allocate_exact_size(vec2(104.0, 96.0), Sense::click_and_drag());
                    if resp.drag_started() && std::path::Path::new(path).exists() {
                        drag = Some(path.clone());
                    }
                    let file = std::path::Path::new(path);
                    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or(path);
                    let resp = resp.on_hover_text(path);
                    let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
                    let down = ui.ctx().animate_bool_with_time(resp.id.with("down"), resp.is_pointer_button_down_on(), 0.06);
                    if hov > 0.0 {
                        ui.painter().rect_filled(cell, Rounding::same(14.0), rgba(255, 255, 255, (20.0 * hov) as u8));
                    }
                    // размер файла узнаём один раз; None — файла уже нет на месте
                    let size = *self.shelf_meta.entry(path.clone()).or_insert_with(|| std::fs::metadata(path).ok().map(|m| if m.is_dir() { u64::MAX } else { m.len() }));
                    let icon = self.app_icons.entry(path.clone()).or_insert_with(|| {
                        sys::icon_rgba(path).map(|(w, h, px)| ui.ctx().load_texture(format!("shelf-{path}"), egui::ColorImage::from_rgba_unmultiplied([w, h], &px), egui::TextureOptions::LINEAR))
                    });
                    // картинки: читаем в фоне один раз, пока читается — обычный значок
                    if shelf_thumb_ok(path, size) && !self.shelf_thumbs.contains_key(path) {
                        self.shelf_thumbs.insert(path.clone(), None);
                        let (q, p, ctx) = (self.shelf_thumb_q.clone(), path.clone(), ui.ctx().clone());
                        std::thread::spawn(move || {
                            let px = load_thumb(&p);
                            lock(&q).push((p, px));
                            ctx.request_repaint();
                        });
                    }
                    let thumb = if size.is_some() { self.shelf_thumbs.get(path).and_then(|t| t.as_ref()) } else { None };
                    let side = 36.0 + 3.0 * hov - 3.0 * down;
                    let icon_r = Rect::from_center_size(pos2(cell.center().x, cell.top() + 30.0), vec2(side, side));
                    match (thumb, icon) {
                        (Some(tex), _) => {
                            let [w, h] = tex.size();
                            let grow = 1.0 + 0.06 * hov - 0.05 * down;
                            let (tw, th) = thumb_fit(w as u32, h as u32, 84.0 * grow, 44.0 * grow);
                            let r = Rect::from_center_size(pos2(cell.center().x, cell.top() + 30.0), vec2(tw, th));
                            // мягкая тень, сама картинка со скруглением и тонкая светлая рамка
                            ui.painter().rect_filled(r.translate(vec2(0.0, 2.0)).expand(1.0), Rounding::same(8.0), rgba(0, 0, 0, 60));
                            rounded_image(ui.painter(), tex, r, 7.0);
                            ui.painter().rect_stroke(r, Rounding::same(7.0), Stroke::new(1.0_f32, rgba(255, 255, 255, (40.0 + 60.0 * hov) as u8)));
                        }
                        (None, Some(tex)) if size.is_some() => rounded_image(ui.painter(), tex, icon_r, 0.0),
                        _ => {
                            ui.painter().text(icon_r.center(), Align2::CENTER_CENTER, "\u{E7C3}", FontId::proportional(26.0), DIM);
                        }
                    }
                    let short: String = if name.chars().count() > 15 { name.chars().take(14).collect::<String>() + "…" } else { name.to_string() };
                    ui.painter().text(pos2(cell.center().x, cell.top() + 62.0), Align2::CENTER_CENTER, short, semi(11.5), if size.is_some() { FG } else { DIM });
                    let info = match size {
                        None => "файл не найден".to_string(),
                        Some(u64::MAX) => "папка".to_string(),
                        Some(b) if b >= 1 << 30 => format!("{:.1} ГБ", b as f64 / (1u64 << 30) as f64).replace('.', ","),
                        Some(b) if b >= 1 << 20 => format!("{:.1} МБ", b as f64 / (1u64 << 20) as f64).replace('.', ","),
                        Some(b) => format!("{} КБ", (b / 1024).max(1)),
                    };
                    ui.painter().text(pos2(cell.center().x, cell.top() + 79.0), Align2::CENTER_CENTER, info, FontId::proportional(10.5), DIM);
                    // крестик в углу появляется под курсором
                    let x = Rect::from_center_size(pos2(cell.right() - 14.0, cell.top() + 14.0), vec2(22.0, 22.0));
                    let del = ui.interact(x, ui.id().with(("del-shelf", i)), Sense::click());
                    if hov > 0.0 || del.hovered() {
                        ui.painter().circle_filled(x.center(), 9.0, rgba(255, 255, 255, if del.hovered() { 60 } else { 26 }));
                        ui.painter().text(x.center(), Align2::CENTER_CENTER, "\u{E711}", FontId::proportional(9.5), FG);
                    }
                    if del.clicked() {
                        remove = Some(i);
                    } else if resp.clicked() {
                        open = Some(path.clone());
                    } else if resp.secondary_clicked() {
                        open = file.parent().and_then(|d| d.to_str()).map(str::to_string);
                    }
                }
            });
        });
        if let Some(path) = drag {
            // Windows сама ведёт перетаскивание до конца; после него сбрасываем состояние мыши,
            // потому что отпускание кнопки досталось другой программе
            sys::drag_file(&path);
            ui.ctx().input_mut(|i| i.pointer = Default::default());
            self.sticky = true;
        }
        if let Some(path) = open {
            sys::open(&path);
        }
        if let Some(i) = remove {
            let path = self.cfg.shelf.remove(i);
            self.shelf_meta.remove(&path);
            self.shelf_thumbs.remove(&path);
            self.cfg.save();
            self.prune_icons();
        }
    }

    /// Выполняет быстрое действие по его имени.
    fn run_action(&mut self, id: &str, ctx: &egui::Context) {
        match id {
            "region" => {
                let seq0 = sys::clipboard_seq();
                sys::open("ms-screenclip:");
                self.mode = Mode::Pill;
                self.notify("Выделите область на экране");
                // когда Windows положит снимок в буфер обмена — сохраняем его и показываем карточку
                let (tx, ctx) = (self.tx.clone(), ctx.clone());
                std::thread::spawn(move || {
                    let start = Instant::now();
                    while start.elapsed() < Duration::from_secs(90) {
                        std::thread::sleep(Duration::from_millis(300));
                        if sys::clipboard_seq() == seq0 {
                            continue;
                        }
                        std::thread::sleep(Duration::from_millis(250));
                        if let Some(img) = arboard::Clipboard::new().ok().and_then(|mut cb| cb.get_image().ok()) {
                            let _ = tx.send(Msg::Shot(img.width, img.height, img.bytes.into_owned()));
                            ctx.request_repaint();
                        }
                        return;
                    }
                });
            }
            "full" => {
                self.mode = Mode::Pill;
                self.shot = Some(6);
            }
            "ocr" => self.ocr_from_snip(ctx),
            "qr" => {
                self.qr_text = self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()).unwrap_or_default().chars().take(400).collect();
                self.tab = Tab::Qr;
            }
            "picker" => {
                self.mode = Mode::Pill;
                self.pick_at = Some(Instant::now() + Duration::from_secs(3));
            }
            "cleantext" => match self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()) {
                Some(text) if !text.is_empty() => {
                    self.set_clipboard(&text);
                    self.notify("Оформление убрано — в буфере чистый текст");
                }
                _ => self.notify("В буфере нет текста"),
            },
            "remind" => self.tab = Tab::Remind,
            "command" => self.toggle_palette(ctx),
            _ => {}
        }
    }

    /// Обзор → Сводка: что сейчас (время, музыка, погода), состояние компьютера, быстрые действия, программы.
    fn ui_home(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        use chrono::Datelike;
        if self.ctl_at.elapsed() > Duration::from_millis(800) && !ui.input(|i| i.pointer.any_down()) {
            self.ctl_at = Instant::now();
            self.vol = sys::volume();
            self.mic = sys::mic_muted();
        }
        let p = ui.painter().clone();

        // ── верхний ряд: «сейчас», музыка, погода
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 100.0), Sense::hover());
        let now_r = Rect::from_min_size(row.min, vec2(190.0, row.height()));
        let weather_r = Rect::from_min_max(pos2(row.right() - 150.0, row.top()), row.max);
        let music_r = Rect::from_min_max(pos2(now_r.right() + 8.0, row.top()), pos2(weather_r.left() - 8.0, row.bottom()));

        {
            let resp = ui.interact(now_r, ui.id().with("home-now"), Sense::click());
            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.12);
            glass_chip(&p, now_r, 20.0, 0.6 + 0.4 * hov);
            let t = chrono::Local::now();
            // небо за часами: рассвет, день, закат или ночь со звёздами — по восходу и закату из погоды
            {
                let hm = |s: &str| s.split_once(':').and_then(|(h, m)| Some(h.trim().parse::<f32>().ok()? + m.trim().parse::<f32>().ok()? / 60.0));
                let rise = self.weather.as_ref().and_then(|w| hm(&w.sunrise)).unwrap_or(7.0);
                let set = self.weather.as_ref().and_then(|w| hm(&w.sunset)).unwrap_or(18.0);
                use chrono::Timelike;
                // час считаем во времени города: восход и закат приходят в его поясе
                let city_t = match &self.weather {
                    Some(w) => chrono::Utc::now().naive_utc() + chrono::Duration::seconds(w.utc_offset as i64),
                    None => t.naive_local(),
                };
                let mut h = city_t.hour() as f32 + city_t.minute() as f32 / 60.0;
                if test_mode() {
                    if let Some(v) = std::env::var("OSTROVOK_HOUR").ok().and_then(|v| v.parse::<f32>().ok()) {
                        h = v;
                    }
                }
                let (top, low) = sky_colors(day_phase(h, rise, set));
                gradient_rrect(&p, now_r.shrink(1.0), 19.0, |q| {
                    let k = ((q.y - now_r.top()) / now_r.height()).clamp(0.0, 1.0);
                    let side = ((q.x - now_r.left()) / now_r.width()).clamp(0.0, 1.0);
                    mix_a(top, low, k * 0.8 + side * 0.2)
                });
                if day_phase(h, rise, set) == 3 {
                    for (i, (x, y)) in [(0.70, 0.18), (0.82, 0.30), (0.90, 0.14), (0.62, 0.36), (0.77, 0.52), (0.93, 0.44), (0.55, 0.12)].into_iter().enumerate() {
                        let at = now_r.min + vec2(now_r.width() * x, now_r.height() * y);
                        p.circle_filled(at, if i % 3 == 0 { 1.4 } else { 0.9 }, rgba(255, 255, 255, if i % 2 == 0 { 170 } else { 110 }));
                    }
                }
            }
            const DAYS: [&str; 7] = ["понедельник", "вторник", "среда", "четверг", "пятница", "суббота", "воскресенье"];
            const MONTHS: [&str; 12] = ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];
            p.text(now_r.min + vec2(16.0, 6.0), Align2::LEFT_TOP, t.format("%H:%M").to_string(), light(38.0), FG);
            let date = format!("{}, {} {}", DAYS[t.weekday().num_days_from_monday() as usize], t.day(), MONTHS[t.month0() as usize]);
            p.text(now_r.min + vec2(17.0, 54.0), Align2::LEFT_TOP, date, FontId::proportional(12.5), FG);
            // ближайшее дело: таймер или напоминание
            let stamp = t.timestamp();
            let next = self.cfg.reminders.iter().filter(|r| r.at > stamp).min_by_key(|r| r.at);
            let todo_left = self.cfg.todos.iter().filter(|t| !t.done).count();
            let (line, color) = if let Some(end) = self.timer_end {
                (format!("\u{E916}  таймер {}", fmt_left(end)), Color32::from_rgb(0xFF, 0xB3, 0x40))
            } else if let Some(r) = next {
                let mins = (r.at - stamp) / 60;
                let when = if mins < 60 { format!("через {} мин", mins.max(1)) } else if mins < 1440 { format!("через {} ч", mins / 60) } else { format!("через {} д", mins / 1440) };
                (format!("\u{E823}  {when} · {}", r.text), accent())
            } else if todo_left > 0 {
                (format!("\u{E73E}  дел в списке: {todo_left}"), accent())
            } else {
                ("\u{E823}  дел на сегодня нет".to_string(), DIM)
            };
            p.with_clip_rect(now_r.shrink2(vec2(12.0, 0.0))).text(now_r.min + vec2(17.0, 75.0), Align2::LEFT_TOP, line, FontId::proportional(11.5), color);
            if resp.clicked() {
                self.tab = if self.timer_end.is_some() { Tab::Timer } else if next.is_some() { Tab::Remind } else if todo_left > 0 { Tab::Todo } else { Tab::Calendar };
            }
        }

        glass_chip(&p, music_r, 20.0, 0.6);
        let art = Rect::from_min_size(music_r.min + vec2(12.0, 12.0), vec2(76.0, 76.0));
        match &self.thumb {
            Some(t) => rounded_image(&p, t, art, 14.0),
            None => match &self.media_now {
                Some(m) => art_placeholder(&p, art, 14.0, &m.title, 26.0),
                None => {
                    p.rect_filled(art, Rounding::same(14.0), tint(20));
                    p.text(art.center(), Align2::CENTER_CENTER, "\u{E8D6}", FontId::proportional(24.0), DIM);
                }
            },
        }
        if ui.interact(art, ui.id().with("home-art"), Sense::click()).on_hover_text("Открыть страницу музыки").clicked() {
            self.tab = Tab::Music;
        }
        let x = art.right() + 14.0;
        match self.media_now.clone() {
            Some(m) => {
                let text = p.with_clip_rect(Rect::from_min_max(pos2(x, music_r.top()), pos2(music_r.right() - 12.0, music_r.bottom())));
                let at = if m.playing { m.pos + m.at.elapsed().as_secs_f64() } else { m.pos };
                let lyric = self.lyrics.iter().rev().find(|l| l.0 <= at).map(|l| l.1.clone());
                text.text(pos2(x, music_r.top() + 13.0), Align2::LEFT_TOP, &m.title, semi(14.0), FG);
                match lyric {
                    Some(line) => text.text(pos2(x, music_r.top() + 33.0), Align2::LEFT_TOP, line, FontId::proportional(12.0), accent()),
                    None => text.text(pos2(x, music_r.top() + 33.0), Align2::LEFT_TOP, &m.artist, FontId::proportional(12.0), DIM),
                };
                if m.dur > 0.0 {
                    let accent = self.album.map_or(FG, |c| Color32::from_rgb(c[0], c[1], c[2]));
                    let track = Rect::from_min_max(pos2(x, music_r.top() + 56.0), pos2(music_r.right() - 14.0, music_r.top() + 60.0));
                    p.rect_filled(track, Rounding::same(2.0), rgba(255, 255, 255, 0x2A));
                    let done = Rect::from_min_max(track.min, pos2(track.left() + track.width() * media::track_frac(at, 0.0, m.dur, false), track.bottom()));
                    p.rect_filled(done, Rounding::same(2.0), accent);
                }
                let mid = (x + music_r.right() - 14.0) / 2.0;
                for (k, (glyph, cmd)) in [("\u{E892}", 0), (if m.playing { "\u{E769}" } else { "\u{E768}" }, 1), ("\u{E893}", 2)].into_iter().enumerate() {
                    let hit = Rect::from_center_size(pos2(mid + (k as f32 - 1.0) * 42.0, music_r.bottom() - 20.0), vec2(30.0, 28.0));
                    let resp = ui.interact(hit, ui.id().with(("media", k)), Sense::click());
                    let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.1);
                    if hov > 0.0 {
                        p.circle_filled(hit.center(), 14.0, rgba(255, 255, 255, (30.0 * hov) as u8));
                    }
                    p.text(hit.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(if cmd == 1 { 16.0 } else { 13.0 }), FG);
                    if resp.clicked() {
                        let _ = self.media_tx.send(match cmd {
                            0 => media::Cmd::Prev,
                            1 => media::Cmd::Toggle,
                            _ => media::Cmd::Next,
                        });
                    }
                }
            }
            None => {
                if self.sound.active() {
                    p.text(pos2(x, music_r.top() + 16.0), Align2::LEFT_TOP, "Играет звук", semi(14.0), FG);
                    let strip = Rect::from_min_max(pos2(x, music_r.top() + 38.0), pos2(music_r.right() - 16.0, music_r.bottom() - 14.0));
                    spectrum(&p, strip, &self.bars, accent(), 0.8, 1.0);
                } else {
                    p.text(pos2(x, music_r.top() + 30.0), Align2::LEFT_TOP, "Ничего не играет", semi(14.0), FG);
                    p.text(pos2(x, music_r.top() + 51.0), Align2::LEFT_TOP, "Включите музыку в плеере", FontId::proportional(12.0), DIM);
                }
            }
        }

        let wresp = ui.interact(weather_r, ui.id().with("home-weather"), Sense::click());
        let whov = ui.ctx().animate_bool_with_time(wresp.id.with("hov"), wresp.hovered(), 0.12);
        glass_chip(&p, weather_r, 20.0, 0.6 + 0.4 * whov);
        match &self.weather {
            Some(w) => {
                let (icon, text) = weather::describe(w.code);
                // свечение в верхнем правом углу — цвета погоды
                let tc = weather::tint(w.code);
                let glow = Color32::from_rgba_unmultiplied(tc[0], tc[1], tc[2], 52);
                let corner = weather_r.right_top();
                let reach = weather_r.width().max(weather_r.height());
                gradient_rrect(&p, weather_r.shrink(1.0), 19.0, |q| {
                    let d = ((q - corner).length() / reach).clamp(0.0, 1.0);
                    mix_a(glow, Color32::TRANSPARENT, d)
                });
                let clip = p.with_clip_rect(weather_r.shrink2(vec2(10.0, 0.0)));
                clip.text(weather_r.min + vec2(16.0, 8.0), Align2::LEFT_TOP, format!("{icon} {}", weather::deg(w.temp)), light(30.0), FG);
                clip.text(weather_r.min + vec2(17.0, 54.0), Align2::LEFT_TOP, text, FontId::proportional(12.5), FG);
                clip.text(weather_r.min + vec2(17.0, 75.0), Align2::LEFT_TOP, &w.city, FontId::proportional(11.5), DIM);
            }
            None => {
                let city_set = self.cfg.lat.is_some();
                let (head, _) = weather::empty_text(city_set, self.weather_err.as_deref());
                let sub = if city_set { head } else { "укажите город" };
                p.text(weather_r.min + vec2(16.0, 30.0), Align2::LEFT_TOP, "Погода", semi(14.0), FG);
                let clip = p.with_clip_rect(weather_r.shrink2(vec2(10.0, 0.0)));
                clip.text(weather_r.min + vec2(16.0, 51.0), Align2::LEFT_TOP, sub, FontId::proportional(12.0), DIM);
            }
        }
        if wresp.clicked() {
            self.tab = if self.weather.is_some() { Tab::Weather } else { Tab::Settings };
        }

        // ── состояние компьютера: два кольца, громкость и микрофон
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::hover());
        let cell = |k: f32, span: f32| {
            let w = (row.width() - 24.0) / 4.0;
            Rect::from_min_size(pos2(row.left() + k * (w + 8.0), row.top()), vec2(w * span + 8.0 * (span - 1.0), row.height()))
        };
        let mem = if self.stats.mem_total > 0.0 { (self.stats.mem_used / self.stats.mem_total) as f32 } else { 0.0 };
        for (k, (name, frac, value)) in [("Процессор", self.stats.cpu, format!("{:.0}%", self.stats.cpu * 100.0)), ("Память", mem, format!("{:.1} ГБ", self.stats.mem_used))].into_iter().enumerate() {
            let r = cell(k as f32, 1.0);
            let resp = ui.interact(r, ui.id().with(("gauge", k)), Sense::click());
            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.12);
            glass_chip(&p, r, 16.0, 0.5 + 0.5 * hov);
            let c = pos2(r.left() + 26.0, r.center().y);
            let shown = ui.ctx().animate_value_with_time(resp.id.with("v"), frac.clamp(0.0, 1.0), 0.5);
            let color = if shown > 0.9 { Color32::from_rgb(0xFF, 0x6B, 0x6B) } else if k == 0 { Color32::from_rgb(0x8F, 0xD3, 0xFF) } else { accent() };
            p.circle_stroke(c, 13.0, Stroke::new(3.5_f32, rgba(255, 255, 255, 26)));
            let steps = (shown * 40.0).ceil().max(1.0) as usize;
            let arc: Vec<Pos2> = (0..=steps)
                .map(|s| {
                    let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * shown * s as f32 / steps as f32;
                    c + vec2(a.cos(), a.sin()) * 13.0
                })
                .collect();
            p.add(egui::Shape::line(arc, Stroke::new(3.5_f32, color)));
            p.text(pos2(r.left() + 50.0, r.center().y - 8.0), Align2::LEFT_CENTER, name, semi(12.5), FG);
            p.text(pos2(r.left() + 50.0, r.center().y + 9.0), Align2::LEFT_CENTER, value, FontId::proportional(11.5), DIM);
            if resp.clicked() {
                self.tab = Tab::System;
            }
        }
        {
            let r = cell(2.0, 1.0);
            glass_chip(&p, r, 16.0, 0.5);
            match self.vol {
                Some((level, muted)) => {
                    let icon = Rect::from_center_size(pos2(r.left() + 24.0, r.center().y), vec2(30.0, 30.0));
                    let resp = ui.interact(icon, ui.id().with("home-mute"), Sense::click()).on_hover_text(if muted { "Включить звук" } else { "Выключить звук" });
                    p.text(icon.center(), Align2::CENTER_CENTER, if muted { "\u{E74F}" } else { "\u{E767}" }, FontId::proportional(17.0), if muted { DIM } else { accent() });
                    if resp.clicked() {
                        sys::set_mute(!muted);
                        self.vol = Some((level, !muted));
                    }
                    p.text(pos2(r.left() + 46.0, r.center().y - 8.0), Align2::LEFT_CENTER, "Громкость", semi(12.5), FG);
                    p.text(pos2(r.right() - 13.0, r.center().y - 8.0), Align2::RIGHT_CENTER, format!("{:.0}%", level * 100.0), FontId::proportional(11.5), DIM);
                    let mut v = level;
                    let track = Rect::from_min_max(pos2(r.left() + 46.0, r.center().y + 3.0), pos2(r.right() - 13.0, r.center().y + 15.0));
                    if slim_slider(ui, ui.id().with("home-vol"), track, &mut v, accent(), muted) {
                        sys::set_volume(v);
                        self.vol = Some((v, muted));
                    }
                }
                None => {
                    p.text(pos2(r.left() + 16.0, r.center().y), Align2::LEFT_CENTER, "Звука нет", semi(12.5), DIM);
                }
            }
        }
        {
            let r = cell(3.0, 1.0);
            let resp = ui.interact(r, ui.id().with("home-mic"), Sense::click());
            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.12);
            glass_chip(&p, r, 16.0, 0.5 + 0.5 * hov);
            let (state, color) = match self.mic {
                Some(true) => ("выключен", Color32::from_rgb(0xFF, 0x6B, 0x6B)),
                Some(false) => ("включён", Color32::from_rgb(0x7B, 0xE0, 0x9A)),
                None => ("не найден", DIM),
            };
            p.text(pos2(r.left() + 24.0, r.center().y), Align2::CENTER_CENTER, "\u{E720}", FontId::proportional(17.0), color);
            p.text(pos2(r.left() + 46.0, r.center().y - 8.0), Align2::LEFT_CENTER, "Микрофон", semi(12.5), FG);
            p.text(pos2(r.left() + 46.0, r.center().y + 9.0), Align2::LEFT_CENTER, state, FontId::proportional(11.5), DIM);
            if resp.clicked() {
                if let Some(m) = self.mic {
                    sys::set_mic_mute(!m);
                    self.mic = Some(!m);
                }
            }
        }

        // ── быстрые действия одной ровной строкой
        const ACTIONS: [(&str, &str, &str, [u8; 3]); 8] = [
            ("region", "\u{E7A8}", "Область", [0x5A, 0xAB, 0xFF]),
            ("full", "\u{E740}", "Весь экран", [0x9B, 0x99, 0xFF]),
            ("ocr", "\u{E8D2}", "Текст с экрана", [0xFF, 0xB3, 0x40]),
            ("qr", "\u{ED14}", "QR-код", [0x4A, 0xE0, 0x6F]),
            ("picker", "\u{EF3C}", "Пипетка", [0xFF, 0x5C, 0x7F]),
            ("cleantext", "\u{E75C}", "Чистый текст", [0x6F, 0xDB, 0xFF]),
            ("remind", "\u{E823}", "Напомнить", [0xFF, 0x8B, 0x61]),
            ("command", "\u{E756}", "Команда", [0xB8, 0xB8, 0xC4]),
        ];
        let mut act = "";
        let (mut hide, mut restore) = (None, false);
        let ids = ACTIONS.map(|a| a.0);
        let shown = visible_actions(&ids, &self.cfg.hidden_actions);
        let has_hidden = shown.len() < ACTIONS.len();
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 60.0), Sense::hover());
        let step = row.width() / (shown.len() + has_hidden as usize) as f32;
        for (slot, &i) in shown.iter().enumerate() {
            let (id, glyph, label, col) = &ACTIONS[i];
            let r = Rect::from_min_size(pos2(row.left() + step * slot as f32, row.top()), vec2(step, row.height()));
            let resp = ui.interact(r, ui.id().with(("act", *id)), Sense::click()).on_hover_text(format!("{} · правый щелчок — скрыть", match *id {
                "region" => "Выделите часть экрана — снимок попадёт в буфер",
                "full" => "Снимок всего экрана: файл в «Изображения\\Островок» и копия в буфере",
                "ocr" => "Выделите область — текст с неё скопируется",
                "qr" => "QR-код из текста или ссылки в буфере",
                "picker" => "Цвет точки под курсором, через 3 секунды",
                "cleantext" => "Убрать оформление у текста в буфере",
                "remind" => "Добавить напоминание",
                _ => "Строка поиска и команд",
            }));
            if resp.secondary_clicked() {
                hide = Some(*id);
            }
            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.12);
            let down = ui.ctx().animate_bool_with_time(resp.id.with("down"), resp.is_pointer_button_down_on(), 0.06);
            let at = pos2(r.center().x, r.top() + 21.0 - 2.0 * hov + 2.0 * down);
            p.circle_filled(at, 18.0 + 1.5 * hov - 1.5 * down, rgba(col[0], col[1], col[2], (36.0 + 40.0 * hov) as u8));
            p.text(at, Align2::CENTER_CENTER, *glyph, FontId::proportional(16.0), Color32::from_rgb(col[0], col[1], col[2]));
            p.text(pos2(r.center().x, r.bottom() - 8.0), Align2::CENTER_CENTER, *label, FontId::proportional(11.0), mix(DIM, FG, hov));
            if resp.clicked() {
                act = id;
            }
        }
        // «+» в конце ряда: возвращает последнее скрытое, в подсказке — весь список
        if has_hidden {
            let r = Rect::from_min_size(pos2(row.left() + step * shown.len() as f32, row.top()), vec2(step, row.height()));
            let names: Vec<&str> = self.cfg.hidden_actions.iter().filter_map(|h| ACTIONS.iter().find(|a| a.0 == h).map(|a| a.2)).collect();
            let resp = ui.interact(r, ui.id().with("act-restore"), Sense::click()).on_hover_text(format!("Скрыто: {}\nЩелчок — вернуть «{}»", names.join(", "), names.last().copied().unwrap_or("")));
            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.12);
            let at = pos2(r.center().x, r.top() + 21.0 - 2.0 * hov);
            p.circle_stroke(at, 17.0 + 1.5 * hov, Stroke::new(1.2_f32, rgba(255, 255, 255, (60.0 + 80.0 * hov) as u8)));
            p.text(at, Align2::CENTER_CENTER, "\u{E710}", FontId::proportional(14.0), mix(DIM, FG, hov));
            p.text(pos2(r.center().x, r.bottom() - 8.0), Align2::CENTER_CENTER, format!("ещё {}", names.len()), FontId::proportional(11.0), mix(DIM, FG, hov));
            restore = resp.clicked();
        }
        if let Some(id) = hide {
            let next = hide_action(&ids, &self.cfg.hidden_actions, id);
            if next.len() == self.cfg.hidden_actions.len() {
                self.notify("Последнее действие скрыть нельзя");
            } else {
                self.cfg.hidden_actions = next;
                self.cfg.save();
                self.notify("Скрыто. Вернуть — кнопкой «+» в конце ряда");
            }
        }
        if restore {
            // старые неизвестные записи снимаем заодно, пока не вернём настоящее действие
            while let Some(h) = self.cfg.hidden_actions.pop() {
                if ids.contains(&h.as_str()) {
                    break;
                }
            }
            self.cfg.save();
        }
        if !act.is_empty() {
            self.run_action(act, ctx);
        }

        // любимые приложения с их настоящими значками: щелчок запускает, правый щелчок убирает
        ui.add_space(2.0);
        let (mut launch, mut remove, mut add) = (None, None, None);
        ui.horizontal(|ui| {
            for (i, app) in self.cfg.apps.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(38.0, 38.0), Sense::click());
                let resp = resp.on_hover_text(format!("{} · правый щелчок — убрать", app.name));
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.12);
                let down = ui.ctx().animate_bool_with_time(resp.id.with("down"), resp.is_pointer_button_down_on(), 0.06);
                glass_chip(ui.painter(), r, 12.0, 0.45 + 0.55 * hov);
                let icon = self.app_icons.entry(app.path.clone()).or_insert_with(|| {
                    sys::icon_rgba(&app.path).map(|(w, h, px)| ui.ctx().load_texture(format!("app-{i}"), egui::ColorImage::from_rgba_unmultiplied([w, h], &px), egui::TextureOptions::LINEAR))
                });
                let size = 24.0 + 2.0 * hov - 2.0 * down;
                match icon {
                    Some(tex) => rounded_image(ui.painter(), tex, Rect::from_center_size(r.center(), vec2(size, size)), 0.0),
                    None => {
                        let letter: String = app.name.chars().take(1).collect();
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, letter.to_uppercase(), semi(15.0), FG);
                    }
                }
                if resp.clicked() {
                    launch = Some(app.path.clone());
                }
                if resp.secondary_clicked() {
                    remove = Some(i);
                }
            }
            match &mut self.app_query {
                Some(q) => {
                    let r = ui.add(
                        egui::TextEdit::singleline(q)
                            .margin(egui::Margin::symmetric(10.0, 9.0))
                            .hint_text(RichText::new("Название программы, Enter").color(DIM))
                            .desired_width(210.0),
                    );
                    r.request_focus();
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        add = Some(q.trim().to_string());
                    } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        add = Some(String::new());
                    }
                }
                None if self.cfg.apps.len() < 12 => {
                    let wide = self.cfg.apps.is_empty();
                    let (r, resp) = ui.allocate_exact_size(vec2(if wide { 210.0 } else { 38.0 }, 38.0), Sense::click());
                    let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.12);
                    glass_chip(ui.painter(), r, 12.0, 0.35 + 0.6 * hov);
                    let label = if wide { "\u{E710}   Закрепить приложение" } else { "\u{E710}" };
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, label, semi(13.0), mix(DIM, FG, hov));
                    if resp.clicked() {
                        self.app_query = Some(String::new());
                    }
                }
                None => {}
            }
        });
        if let Some(path) = launch {
            sys::open(&path);
            self.mode = Mode::Pill;
        }
        if let Some(i) = remove {
            self.cfg.apps.remove(i);
            self.cfg.save();
        }
        if let Some(q) = add {
            self.app_query = None;
            if !q.is_empty() {
                let low = q.to_lowercase();
                let found = {
                    let apps = lock(&self.apps);
                    let mut hits: Vec<&(String, String)> = apps.iter().filter(|a| a.0.to_lowercase().contains(&low)).collect();
                    hits.sort_by_key(|a| (!a.0.to_lowercase().starts_with(&low), a.0.len()));
                    hits.first().map(|a| (a.0.clone(), a.1.clone()))
                };
                match found {
                    Some((name, path)) => {
                        self.cfg.apps.retain(|a| a.path != path);
                        self.cfg.apps.push(store::PinnedApp { name, path });
                        self.cfg.save();
                    }
                    None => self.notify(&format!("Программа «{q}» не найдена")),
                }
            }
        }

        // ── лента дня: сутки полосой, «сейчас» и сегодняшние напоминания точками
        ui.add_space(6.0);
        let (strip, sresp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
        let now = chrono::Local::now();
        let midnight = now.date_naive().and_hms_opt(0, 0, 0).and_then(|m| m.and_local_timezone(chrono::Local).earliest()).map_or(now.timestamp() - 43_200, |m| m.timestamp());
        // в день перевода часов сутки длятся 23 или 25 часов
        let day_len = now.date_naive().succ_opt().and_then(|d| d.and_hms_opt(0, 0, 0)).and_then(resolve_here).map(|m| m.timestamp() - midnight).filter(|l| *l > 0).unwrap_or(86_400);
        let marks = day_marks(&self.cfg.reminders, midnight, day_len);
        let frac_now = ((now.timestamp() - midnight) as f32 / day_len as f32).clamp(0.0, 1.0);
        let left_n = marks.iter().filter(|m| m.0 > frac_now).count();
        let shov = ui.ctx().animate_bool_with_time(sresp.id.with("hov"), sresp.hovered(), 0.12);
        p.text(pos2(strip.left() + 2.0, strip.center().y), Align2::LEFT_CENTER, "Сегодня", semi(11.5), mix(DIM, FG, shov));
        let tail = if left_n > 0 { format!("ещё {}", dela_word(left_n)) } else if marks.is_empty() { "день свободен".into() } else { "всё позади".into() };
        p.text(pos2(strip.right() - 2.0, strip.center().y), Align2::RIGHT_CENTER, tail, FontId::proportional(11.0), if left_n > 0 { accent() } else { DIM });
        let track = Rect::from_min_max(pos2(strip.left() + 62.0, strip.center().y - 2.0), pos2(strip.right() - 92.0, strip.center().y + 2.0));
        if track.width() > 40.0 {
            p.rect_filled(track, Rounding::same(2.0), rgba(255, 255, 255, (28.0 + 14.0 * shov) as u8));
            let x_of = |f: f32| track.left() + track.width() * f;
            let done = Rect::from_min_max(track.min, pos2(x_of(frac_now), track.bottom()));
            let a = accent();
            if done.width() > 1.0 { gradient_rrect(&p, done, 2.0, |q| mix_a(Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), 40), a, ((q.x - track.left()) / done.width()).clamp(0.0, 1.0))); }
            for h in [6.0_f32, 12.0, 18.0] {
                p.circle_filled(pos2(x_of(h / 24.0), track.center().y), 1.2, rgba(255, 255, 255, 70));
            }
            let pointer = ui.input(|i| i.pointer.hover_pos());
            let mut tip: Option<String> = None;
            for (f, text) in &marks {
                let c = pos2(x_of(*f), track.center().y);
                let past = *f <= frac_now;
                p.circle_filled(c, 4.5, if past { rgba(255, 255, 255, 60) } else { Color32::from_rgb(0xFF, 0x8B, 0x61) });
                p.circle_stroke(c, 4.5, Stroke::new(1.0_f32, rgba(0, 0, 0, 90)));
                if pointer.is_some_and(|q| (q - c).length() < 8.0) {
                    let secs = (*f * 86_400.0).round() as u32;
                    tip = Some(format!("{:02}:{:02} · {text}", secs / 3600, secs / 60 % 60));
                }
            }
            let nx = x_of(frac_now);
            p.circle_filled(pos2(nx, track.center().y), 7.0, Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), 50));
            p.circle_filled(pos2(nx, track.center().y), 3.5, FG);
            let sresp = sresp.on_hover_text(tip.unwrap_or_else(|| format!("Сейчас {} · щелчок — напоминания", now.format("%H:%M"))));
            if sresp.clicked() {
                self.tab = Tab::Remind;
            }
        }
    }

    /// Обзор → Музыка: большая обложка, управление и текст песни строчка за строчкой.
    fn ui_music(&mut self, ui: &mut egui::Ui) {
        let accent = self.album.map_or(accent(), |c| Color32::from_rgb(c[0], c[1], c[2]));
        let Some(m) = self.media_now.clone() else {
            if self.sound.active() {
                // кто звучит — спрашиваем у Windows не чаще раза в полторы секунды
                if self.sounding.1.elapsed() > Duration::from_millis(1500) {
                    let names: Vec<String> = sys::app_volumes().into_iter().filter(|a| a.active && !a.muted).map(|a| a.name).take(3).collect();
                    self.sounding = (names.join(", "), Instant::now());
                }
                ui.add_space(26.0);
                let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), 170.0), Sense::hover());
                let p = ui.painter().clone();
                glass_chip(&p, area, 20.0, 0.4);
                spectrum(&p.with_clip_rect(area), area.shrink2(vec2(18.0, 0.0)), &self.bars, accent, 0.85, 0.72);
                ui.add_space(12.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(if self.sounding.0.is_empty() { "Играет звук компьютера".to_string() } else { format!("Звучит: {}", self.sounding.0) }).font(semi(16.5)));
                    ui.label(RichText::new("Плеер не сообщил название трека, поэтому остров показывает сам звук.").color(DIM));
                });
                return;
            }
            ui.add_space(70.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("\u{E8D6}").size(34.0).color(DIM));
                ui.label(RichText::new("Ничего не играет").font(semi(16.5)));
                ui.label(RichText::new("Включите музыку в Spotify, браузере или любом плеере — она появится здесь.").color(DIM));
            });
            return;
        };
        let at = (if m.playing { m.pos + m.at.elapsed().as_secs_f64() } else { m.pos }).min(if m.dur > 0.0 { m.dur } else { f64::MAX });
        let clock = |v: f64| format!("{}:{:02}", v as u64 / 60, v as u64 % 60);

        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 104.0), Sense::hover());
        let p = ui.painter().clone();
        let art = Rect::from_min_size(row.min, vec2(104.0, 104.0));
        match &self.thumb {
            Some(t) => rounded_image(&p, t, art, 20.0),
            None => art_placeholder(&p, art, 20.0, &m.title, 36.0),
        }
        let x = art.right() + 18.0;
        let text = p.with_clip_rect(Rect::from_min_max(pos2(x, row.top()), row.max));
        text.text(pos2(x, row.top() + 4.0), Align2::LEFT_TOP, &m.title, semi(18.0), FG);
        text.text(pos2(x, row.top() + 30.0), Align2::LEFT_TOP, &m.artist, FontId::proportional(13.5), DIM);
        // полоса прогресса со временем
        // щелчок или перетаскивание по полосе перематывают трек
        let bar = Rect::from_min_max(pos2(x, row.top() + 62.0), pos2(row.right(), row.top() + 67.0));
        let seek = ui.interact(bar.expand2(vec2(6.0, 8.0)), ui.id().with("music-seek"), if m.dur > 0.0 { Sense::click_and_drag() } else { Sense::hover() });
        let grab = ui.ctx().animate_bool_with_time(seek.id.with("hov"), m.dur > 0.0 && (seek.hovered() || seek.dragged()), 0.12);
        let track = bar.expand2(vec2(0.0, 1.5 * grab));
        p.rect_filled(track, Rounding::same(2.5 + 1.5 * grab), rgba(255, 255, 255, 0x33));
        let mut at = at;
        // при перетаскивании мышь может уйти с полосы — тогда берём её последнее положение
        let busy = seek.dragged() || seek.drag_stopped() || seek.clicked();
        let ptr = if busy { ui.ctx().pointer_latest_pos() } else { seek.hover_pos() };
        if let Some(ptr) = ptr.filter(|_| m.dur > 0.0) {
            let to = media::seek_secs(ptr.x, track.left(), track.width(), m.dur);
            if let Some(to) = to {
                // где окажемся — время над курсором
                let tip = pos2(ptr.x.clamp(track.left() + 16.0, track.right() - 16.0), track.top() - 12.0);
                let g = p.layout_no_wrap(clock(to), FontId::proportional(11.0), FG);
                let r = Rect::from_center_size(tip, g.size() + vec2(10.0, 4.0));
                p.rect_filled(r, Rounding::same(6.0), rgba(10, 10, 18, (210.0 * grab) as u8));
                p.galley(r.min + vec2(5.0, 2.0), g, FG);
                if seek.dragged() || seek.clicked() {
                    at = to; // ползунок едет за мышью сразу
                }
                if seek.drag_stopped() || seek.clicked() {
                    let _ = self.media_tx.send(media::Cmd::Seek(to));
                    // не ждём опроса плеера: позиция обновляется сразу
                    if let Some(info) = self.media.lock().unwrap().info.as_mut() {
                        info.pos = to;
                        info.at = Instant::now();
                    }
                }
            }
        }
        if m.dur > 0.0 {
            let done = Rect::from_min_max(track.min, pos2(track.left() + track.width() * media::track_frac(at, 0.0, m.dur, false), track.bottom()));
            p.rect_filled(done, Rounding::same(2.5 + 1.5 * grab), accent);
            p.circle_filled(pos2(done.right(), track.center().y), 5.0 + 2.0 * grab, FG);
            p.text(pos2(x, track.bottom() + 6.0), Align2::LEFT_TOP, clock(at), FontId::proportional(11.5), DIM);
            p.text(pos2(row.right(), track.bottom() + 6.0), Align2::RIGHT_TOP, clock(m.dur), FontId::proportional(11.5), DIM);
        }
        // управление
        let mid = pos2((x + row.right()) / 2.0, row.bottom() - 9.0);
        for (k, (glyph, cmd)) in [("\u{E892}", 0), (if m.playing { "\u{E769}" } else { "\u{E768}" }, 1), ("\u{E893}", 2)].into_iter().enumerate() {
            let hit = Rect::from_center_size(pos2(mid.x + (k as f32 - 1.0) * 54.0, mid.y), vec2(40.0, 34.0));
            let resp = ui.interact(hit, ui.id().with(("music", k)), Sense::click());
            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.1);
            let down = ui.ctx().animate_bool_with_time(resp.id.with("down"), resp.is_pointer_button_down_on(), 0.06);
            if cmd == 1 {
                p.circle_filled(hit.center(), 17.0 + hov - 1.5 * down, FG);
                p.text(hit.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(15.0), BG);
            } else {
                if hov > 0.0 {
                    p.circle_filled(hit.center(), 16.0, rgba(255, 255, 255, (34.0 * hov) as u8));
                }
                p.text(hit.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(15.0), FG);
            }
            if resp.clicked() {
                let _ = self.media_tx.send(match cmd {
                    0 => media::Cmd::Prev,
                    1 => media::Cmd::Toggle,
                    _ => media::Cmd::Next,
                });
            }
        }

        // текст песни: текущая строка яркая, соседние гаснут
        ui.add_space(6.0);
        let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), 132.0), Sense::hover());
        glass_chip(&p, area, 18.0, 0.4);
        let no_text = self.cfg.lyrics && self.lyrics.is_empty();
        if !no_text {
            spectrum(&p.with_clip_rect(area), area.shrink2(vec2(14.0, 0.0)), &self.bars, accent, 0.26, 0.62);
        }
        let lines = p.with_clip_rect(area.shrink(8.0));
        if !self.cfg.lyrics {
            lines.text(area.center() - vec2(0.0, 16.0), Align2::CENTER_CENTER, "Текст песни выключен", semi(14.0), FG);
            lines.text(area.center() + vec2(0.0, 4.0), Align2::CENTER_CENTER, "Для него исполнитель и название трека отправляются на lrclib.net", FontId::proportional(12.0), DIM);
            let b = Rect::from_center_size(area.center() + vec2(0.0, 36.0), vec2(150.0, 30.0));
            let resp = ui.interact(b, ui.id().with("lyrics-on"), Sense::click());
            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.12);
            glass_chip(&p, b, 12.0, 0.6 + 0.5 * hov);
            p.text(b.center(), Align2::CENTER_CENTER, "Включить текст", semi(13.0), FG);
            if resp.clicked() {
                self.cfg.lyrics = true;
                self.cfg.save();
                self.lyrics_key.clear();
            }
        } else if self.lyrics.is_empty() {
            // текста нет — вместо пустой рамки крупный зеркальный эквалайзер в цвет обложки
            let t = ui.input(|i| i.time) as f32;
            spectrum_mirror(&p.with_clip_rect(area), area.shrink2(vec2(18.0, 26.0)).translate(vec2(0.0, 8.0)), &self.bars, accent, t, m.playing);
            lines.text(pos2(area.center().x, area.top() + 16.0), Align2::CENTER_CENTER, "Текста для этого трека не нашлось — только звук", FontId::proportional(11.5), DIM);
        } else {
            let cur = self.lyrics.iter().rposition(|l| l.0 <= at);
            // плавная прокрутка: строка въезжает на место за время до следующей
            let slide = match cur {
                Some(i) => {
                    let start = self.lyrics[i].0;
                    (((at - start) / 0.35).clamp(0.0, 1.0) as f32 - 1.0) * -24.0
                }
                None => 0.0,
            };
            let base = cur.map_or(-1, |i| i as i32);
            for off in -2..=3 {
                let i = base + off;
                if i < 0 || i as usize >= self.lyrics.len() {
                    continue;
                }
                let y = area.center().y + off as f32 * 24.0 + slide - 6.0;
                let far = (off as f32).abs();
                let (font, color) = if off == 0 { (semi(15.5), FG) } else { (FontId::proportional(13.5), mix(DIM, BG, (far - 1.0) * 0.4 + 0.15)) };
                lines.text(pos2(area.center().x, y), Align2::CENTER_CENTER, &self.lyrics[i as usize].1, font, color);
            }
        }
    }

    /// Обзор → Управление: сеть, звук, питание и мелкие удобства Windows.
    fn ui_control(&mut self, ui: &mut egui::Ui) {
        if self.ctl_at.elapsed() > Duration::from_millis(600) || self.vol.is_none() {
            self.ctl_at = Instant::now();
            self.vol = sys::volume();
            self.mic = sys::mic_muted();
            self.bin = sys::recycle_bin();
            self.light_theme = sys::light_theme();
        }
        let armed = self.confirm.filter(|c| c.1 > Instant::now()).map(|c| c.0);
        let mut act = "";
        let p = ui.painter().clone();
        let red = Color32::from_rgb(0xFF, 0x6B, 0x6B);

        // ── круглые переключатели одной строкой: включённые залиты цветом темы
        let pinned = self.last_fg != 0 && sys::is_topmost(self.last_fg);
        let win_title: String = if self.last_fg == 0 { "нет окна".into() } else { sys::window_title(self.last_fg).chars().take(13).collect() };
        let bin_armed = armed == Some("bin");
        let bin_state = if self.bin.0 == 0 { "пусто".to_string() } else if bin_armed { "ещё раз".to_string() } else { format!("{} · {:.0} МБ", self.bin.0, self.bin.1 as f64 / 1048576.0) };
        let toggles: [(&str, &str, &str, String, bool); 8] = [
            ("wifi", "\u{E701}", "Wi-Fi", "настройки".into(), false),
            ("bt", "\u{E702}", "Bluetooth", "настройки".into(), false),
            ("vpn", "\u{E705}", "VPN", "настройки".into(), false),
            ("theme", "\u{E706}", "Светлая тема", if self.light_theme == Some(true) { "включена" } else { "выключена" }.into(), self.light_theme == Some(true)),
            ("awake", "\u{E7E8}", "Не засыпать", if self.awake { "включено" } else { "выключено" }.into(), self.awake),
            (
                "mic",
                "\u{E720}",
                "Микрофон",
                match self.mic {
                    Some(true) => "выключен",
                    Some(false) => "включён",
                    None => "не найден",
                }
                .into(),
                self.mic == Some(false),
            ),
            ("pin", "\u{E718}", "Поверх всех", win_title, pinned),
            ("bin", "\u{E74D}", "Корзина", bin_state, bin_armed),
        ];
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 88.0), Sense::hover());
        let step = row.width() / toggles.len() as f32;
        for (i, (id, glyph, title, state, on)) in toggles.iter().enumerate() {
            let r = Rect::from_min_size(pos2(row.left() + step * i as f32, row.top()), vec2(step, row.height()));
            let resp = ui.interact(r, ui.id().with(("ctl", *id)), Sense::click());
            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.12);
            let down = ui.ctx().animate_bool_with_time(resp.id.with("down"), resp.is_pointer_button_down_on(), 0.06);
            let lit = ui.ctx().animate_bool_with_time(resp.id.with("on"), *on, 0.16);
            let c = pos2(r.center().x, r.top() + 27.0 - 1.5 * hov + 1.5 * down);
            let rad = 22.0 + 1.5 * hov - 1.5 * down;
            let tone = if *id == "bin" { red } else { accent() };
            p.circle_filled(c, rad, rgba(255, 255, 255, (20.0 + 22.0 * hov) as u8));
            if lit > 0.0 {
                p.circle_filled(c, rad, Color32::from_rgba_unmultiplied(tone.r(), tone.g(), tone.b(), (lit * 225.0) as u8));
            }
            p.circle_stroke(c, rad, Stroke::new(1.0_f32, rgba(255, 255, 255, 34)));
            p.text(c, Align2::CENTER_CENTER, *glyph, FontId::proportional(17.0), mix(FG, Color32::from_rgb(16, 14, 28), lit));
            p.text(pos2(r.center().x, r.top() + 62.0), Align2::CENTER_CENTER, *title, semi(11.5), FG);
            p.with_clip_rect(r).text(pos2(r.center().x, r.top() + 78.0), Align2::CENTER_CENTER, state, FontId::proportional(10.5), mix(DIM, tone, lit));
            if resp.clicked() {
                act = id;
            }
        }

        // ── нижняя строка: папки слева, питание одной капсулой справа
        ui.add_space(6.0);
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
        let cap = Rect::from_min_max(pos2(row.right() - 4.0 * 42.0 - 8.0, row.top()), row.max);
        glass_chip(&p, cap, 20.0, 0.55);
        for (k, (id, glyph, name)) in [("lock", "\u{E72E}", "Блокировка"), ("sleep", "\u{E708}", "Сон"), ("restart", "\u{E777}", "Перезагрузка"), ("off", "\u{E7E8}", "Выключение")].into_iter().enumerate() {
            let hit = Rect::from_center_size(pos2(cap.left() + 25.0 + k as f32 * 42.0, cap.center().y), vec2(38.0, 34.0));
            let hot = armed == Some(id);
            let tip = if hot { format!("{name}: нажмите ещё раз") } else if id == "lock" { name.to_string() } else { format!("{name} — нужно два нажатия") };
            let resp = ui.interact(hit, ui.id().with(("pw", id)), Sense::click()).on_hover_text(tip);
            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
            if hot {
                p.circle_filled(hit.center(), 15.0, red);
            } else if hov > 0.0 {
                p.circle_filled(hit.center(), 15.0, rgba(255, 255, 255, (34.0 * hov) as u8));
            }
            p.text(hit.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(15.0), FG);
            if resp.clicked() {
                act = id;
            }
        }
        let mut folders = ui.new_child(
            egui::UiBuilder::new().max_rect(Rect::from_min_max(row.min, pos2(cap.left() - 10.0, row.bottom()))).layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        folders.label(RichText::new("Папки").color(DIM));
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        for (name, dir) in [("Загрузки", "Downloads"), ("Документы", "Documents"), ("Рабочий стол", "Desktop"), ("Изображения", "Pictures")] {
            if jelly(&mut folders, name).clicked() {
                sys::open(&format!("{home}\\{dir}"));
                self.mode = Mode::Pill;
            }
        }

        // ── отложить: блокировка или сон через N минут, отмена одним щелчком
        ui.add_space(6.0);
        {
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
            let moon = Color32::from_rgb(0xB4, 0x9C, 0xFF);
            let chip = |ui: &egui::Ui, r: Rect, key: &str, label: &str, lit: bool, tone: Color32| -> bool {
                let resp = ui.interact(r, ui.id().with(("pt", key)), Sense::click());
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
                let glow = ui.ctx().animate_bool_with_time(resp.id.with("lit"), lit, 0.16);
                glass_chip(&p, r, 16.0, 0.4 + 0.5 * hov);
                if glow > 0.0 {
                    p.rect_filled(r.shrink(1.0), Rounding::same(15.0), Color32::from_rgba_unmultiplied(tone.r(), tone.g(), tone.b(), (70.0 * glow) as u8));
                }
                p.text(r.center(), Align2::CENTER_CENTER, label, semi(12.0), mix(mix(DIM, FG, hov), FG, glow));
                resp.clicked()
            };
            p.text(pos2(row.left() + 4.0, row.center().y), Align2::LEFT_CENTER, "\u{E916}  Отложить", semi(12.5), DIM);
            let x0 = row.left() + 100.0;
            match self.power_timer {
                Some((action, at, _)) => {
                    let left = at.saturating_duration_since(Instant::now()).as_secs();
                    let r = Rect::from_min_max(pos2(x0, row.top()), pos2(row.right() - 104.0, row.bottom()));
                    glass_chip(&p, r, 16.0, 0.55);
                    p.rect_filled(r.shrink(1.0), Rounding::same(15.0), Color32::from_rgba_unmultiplied(moon.r(), moon.g(), moon.b(), 46));
                    p.text(pos2(r.left() + 16.0, r.center().y), Align2::LEFT_CENTER, power_timer_label(action, left), semi(12.5), FG);
                    if chip(ui, Rect::from_min_max(pos2(row.right() - 96.0, row.top()), row.max), "cancel", "Отменить", false, moon) {
                        self.power_timer = None;
                        self.notify("Отложенное действие отменено");
                    }
                    // секунды идут, пока страница открыта
                    ui.ctx().request_repaint_after(Duration::from_millis(500));
                }
                None => {
                    let mut x = x0;
                    for (key, label, w) in [("lock", "Блокировка", 96.0), ("sleep", "Сон", 56.0)] {
                        if chip(ui, Rect::from_min_size(pos2(x, row.top()), vec2(w, row.height())), key, label, self.power_pick == key, moon) {
                            self.power_pick = if key == "lock" { "lock" } else { "sleep" };
                        }
                        x += w + 6.0;
                    }
                    x += 10.0;
                    let step = (row.right() - x) / 4.0;
                    for (k, m) in [15u64, 30, 60, 90].into_iter().enumerate() {
                        let r = Rect::from_min_size(pos2(x + step * k as f32, row.top()), vec2(step - 6.0, row.height()));
                        if chip(ui, r, &format!("m{m}"), &format!("{m} мин"), false, accent()) {
                            self.power_timer = Some((self.power_pick, Instant::now() + Duration::from_secs(m * 60), false));
                            self.notify(&format!("{} через {m} мин", if self.power_pick == "sleep" { "Сон" } else { "Блокировка" }));
                        }
                    }
                }
            }
        }

        let arm = |me: &mut Self, id: &'static str| me.confirm = Some((id, Instant::now() + Duration::from_secs(4)));
        match act {
            "wifi" => sys::open("ms-settings:network-wifi"),
            "bt" => sys::open("ms-settings:bluetooth"),
            "vpn" => sys::open("ms-settings:network-vpn"),
            "theme" => {
                let light = !self.light_theme.unwrap_or(false);
                sys::set_light_theme(light);
                self.light_theme = Some(light);
            }
            "awake" => {
                self.awake = !self.awake;
                sys::keep_awake(self.awake);
                self.notify(if self.awake { "Компьютер не уснёт, пока включено" } else { "Обычный режим сна" });
            }
            "mic" => {
                if let Some(m) = self.mic {
                    sys::set_mic_mute(!m);
                    self.mic = Some(!m);
                }
            }
            "pin" => {
                if self.last_fg != 0 {
                    sys::set_topmost(self.last_fg, !pinned);
                    self.notify(if pinned { "Окно больше не закреплено" } else { "Окно закреплено поверх остальных" });
                }
            }
            "bin" => {
                if self.bin.0 > 0 {
                    if armed == Some("bin") {
                        self.confirm = None;
                        sys::empty_recycle_bin();
                        self.bin = (0, 0);
                        self.notify("Корзина очищена");
                    } else {
                        arm(self, "bin");
                    }
                }
            }
            "lock" => sys::power("lock"),
            "sleep" | "restart" | "off" => {
                if armed == Some(act) {
                    self.confirm = None;
                    store::save_notes(&self.notes);
                    sys::power(act);
                } else {
                    arm(self, match act {
                        "sleep" => "sleep",
                        "restart" => "restart",
                        _ => "off",
                    });
                }
            }
            _ => {}
        }
    }

    /// Система → Звук: слева ручка общей громкости и микрофон, справа — программы со своими ползунками.
    fn ui_sound(&mut self, ui: &mut egui::Ui) {
        if self.mixer_at.elapsed() > Duration::from_millis(900) && !ui.input(|i| i.pointer.any_down()) {
            self.mixer_at = Instant::now();
            self.vol = sys::volume();
            self.mic = sys::mic_muted();
            self.mixer = sys::app_volumes();
        }
        let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), 190.0), Sense::hover());
        let p = ui.painter().clone();
        let red = Color32::from_rgb(0xFF, 0x6B, 0x6B);

        // ── ручка громкости: тянуть по кругу, колесо — по два процента, щелчок — без звука
        let c = pos2(area.left() + 84.0, area.top() + 76.0);
        match self.vol {
            Some((level, muted)) => {
                let resp = ui
                    .interact(Rect::from_center_size(c, vec2(152.0, 152.0)), ui.id().with("knob"), Sense::click_and_drag())
                    .on_hover_text(if muted { "Щелчок — включить звук" } else { "Тяните по кругу или крутите колесо; щелчок — без звука" });
                let mut v = level;
                if resp.dragged() {
                    if let Some(pt) = resp.interact_pointer_pos() {
                        let d = pt - c;
                        if d.length() > 14.0 {
                            let mut f = d.x.atan2(-d.y) / std::f32::consts::TAU;
                            if f < 0.0 {
                                f += 1.0;
                            }
                            // через верхнюю точку ручка не перескакивает с нуля на сто и обратно
                            v = if (f - level).abs() < 0.5 { f } else if level > 0.5 { 1.0 } else { 0.0 };
                        }
                    }
                } else if resp.clicked() {
                    sys::set_mute(!muted);
                    self.vol = Some((level, !muted));
                } else if resp.hovered() {
                    let wheel = ui.input(|i| i.raw_scroll_delta.y);
                    if wheel != 0.0 {
                        v = (level + wheel.signum() * 0.02).clamp(0.0, 1.0);
                    }
                }
                if (v - level).abs() > 0.002 {
                    sys::set_volume(v);
                    self.vol = Some((v, muted));
                    // громкость выбрали рукой — тихий режим больше не держим
                    self.quiet_prev = None;
                }
                let hot = ui.ctx().animate_bool_with_time(resp.id.with("hot"), resp.hovered() || resp.dragged(), 0.12);
                let tone = if muted { Color32::from_rgb(120, 120, 134) } else { accent() };
                ring(&p, c, 62.0, 8.0 + 2.0 * hot, v, tone);
                let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * v;
                p.circle_filled(c + vec2(a.cos(), a.sin()) * 62.0, 6.0 + 2.0 * hot, Color32::WHITE);
                p.text(c - vec2(0.0, 6.0), Align2::CENTER_CENTER, format!("{:.0}", v * 100.0), light(34.0), if muted { DIM } else { FG });
                p.text(c + vec2(0.0, 24.0), Align2::CENTER_CENTER, if muted { "без звука" } else { "громкость" }, FontId::proportional(11.5), if muted { red } else { DIM });
            }
            None => {
                ring(&p, c, 62.0, 8.0, 0.0, DIM);
                p.text(c, Align2::CENTER_CENTER, "нет звука", FontId::proportional(12.0), DIM);
            }
        }
        // ── микрофон под ручкой: строка без рамки
        {
            let r = Rect::from_center_size(pos2(c.x, area.bottom() - 14.0), vec2(156.0, 28.0));
            let resp = ui.interact(r, ui.id().with("mic"), Sense::click());
            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.12);
            let (state, color) = match self.mic {
                Some(true) => ("микрофон выключен", red),
                Some(false) => ("микрофон включён", Color32::from_rgb(0x7B, 0xE0, 0x9A)),
                None => ("микрофон не найден", DIM),
            };
            if hov > 0.0 {
                p.rect_filled(r, Rounding::same(14.0), rgba(255, 255, 255, (22.0 * hov) as u8));
            }
            p.text(pos2(r.left() + 18.0, r.center().y), Align2::CENTER_CENTER, "\u{E720}", FontId::proportional(14.0), color);
            p.text(pos2(r.left() + 32.0, r.center().y), Align2::LEFT_CENTER, state, FontId::proportional(12.0), mix(DIM, FG, hov));
            if resp.clicked() {
                if let Some(m) = self.mic {
                    sys::set_mic_mute(!m);
                    self.mic = Some(!m);
                }
            }
        }

        // ── ряд громкости: «Тихо» (20% и обратно) и четыре пресета
        {
            ui.add_space(6.0);
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::hover());
            let level = self.vol.map(|v| v.0);
            let on = level.is_some();
            let current = level.and_then(preset_index);
            let moon = Color32::from_rgb(0xB4, 0x9C, 0xFF);
            let quiet_w = 96.0;
            let step = (row.width() - quiet_w - 8.0) / VOL_PRESETS.len() as f32;
            let mut pick: Option<f32> = None;
            let mut toggle = false;
            for k in 0..=VOL_PRESETS.len() {
                let r = if k == 0 {
                    Rect::from_min_size(row.min, vec2(quiet_w, row.height()))
                } else {
                    Rect::from_min_size(pos2(row.left() + quiet_w + 8.0 + step * (k - 1) as f32, row.top()), vec2(step - 6.0, row.height()))
                };
                let resp = ui.interact(r, ui.id().with(("vol-chip", k)), if on { Sense::click() } else { Sense::hover() });
                let resp = if k == 0 { resp.on_hover_text(if self.quiet_prev.is_some() { "Вернуть прежнюю громкость" } else { "Тихо: громкость 20%, повторно — как было" }) } else { resp };
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered() && on, 0.12);
                let lit = if k == 0 { self.quiet_prev.is_some() } else { current == Some(k - 1) && self.quiet_prev.is_none() };
                let glow = ui.ctx().animate_bool_with_time(resp.id.with("lit"), lit, 0.18);
                glass_chip(&p, r, 15.0, 0.4 + 0.5 * hov);
                let tone = if k == 0 { moon } else { accent() };
                if glow > 0.0 {
                    p.rect_filled(r.shrink(1.0), Rounding::same(14.0), Color32::from_rgba_unmultiplied(tone.r(), tone.g(), tone.b(), (60.0 * glow) as u8));
                }
                let ink = if !on { DIM } else { mix(mix(DIM, FG, hov), FG, glow) };
                if k == 0 {
                    p.text(pos2(r.left() + 20.0, r.center().y), Align2::CENTER_CENTER, "\u{E708}", FontId::proportional(13.0), if on { moon } else { DIM });
                    p.text(pos2(r.left() + 36.0, r.center().y), Align2::LEFT_CENTER, if self.quiet_prev.is_some() { "Как было" } else { "Тихо" }, semi(12.0), ink);
                } else {
                    p.text(r.center(), Align2::CENTER_CENTER, format!("{:.0}%", VOL_PRESETS[k - 1] * 100.0), semi(12.0), ink);
                }
                if resp.clicked() {
                    if k == 0 { toggle = true } else { pick = Some(VOL_PRESETS[k - 1]) }
                }
            }
            if let Some((lv, muted)) = self.vol {
                let target = if toggle {
                    let (v, saved) = quiet_toggle(lv, self.quiet_prev);
                    self.quiet_prev = saved;
                    Some(v)
                } else if let Some(v) = pick {
                    self.quiet_prev = None;
                    Some(v)
                } else {
                    None
                };
                if let Some(v) = target {
                    sys::set_volume(v);
                    if muted {
                        sys::set_mute(false);
                    }
                    self.vol = Some((v, false));
                }
            }
        }

        // ── программы: простые строки с тонкими разделителями, без рамок
        let list = Rect::from_min_max(pos2(area.left() + 190.0, area.top()), area.max);
        p.line_segment([pos2(list.left() - 12.0, area.top() + 8.0), pos2(list.left() - 12.0, area.bottom() - 8.0)], Stroke::new(1.0_f32, rgba(255, 255, 255, 22)));
        if self.mixer.is_empty() {
            empty_at(&p, list.center() + vec2(0.0, 10.0), "\u{E767}", "Тишина", "Сейчас ни одна программа не выводит звук");
            return;
        }
        let (mut set, mut mute): (Option<(usize, f32)>, Option<usize>) = (None, None);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list));
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut child, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for i in 0..self.mixer.len() {
                let app = &self.mixer[i];
                let (row, _) = ui.allocate_exact_size(vec2(ui.available_width() - 6.0, 38.0), Sense::hover());
                let p = ui.painter();
                if i > 0 {
                    p.line_segment([pos2(row.left() + 36.0, row.top()), pos2(row.right(), row.top())], Stroke::new(1.0_f32, rgba(255, 255, 255, 14)));
                }
                let icon_r = Rect::from_center_size(pos2(row.left() + 14.0, row.center().y), vec2(22.0, 22.0));
                let resp = ui.interact(icon_r.expand(6.0), ui.id().with(("mix-mute", i)), Sense::click()).on_hover_text(if app.muted { "Включить звук программы" } else { "Заглушить программу" });
                let icon = self.app_icons.entry(app.path.clone()).or_insert_with(|| {
                    sys::icon_rgba(&app.path).map(|(w, h, px)| ui.ctx().load_texture(format!("mix-{}", app.name), egui::ColorImage::from_rgba_unmultiplied([w, h], &px), egui::TextureOptions::LINEAR))
                });
                match icon {
                    Some(tex) => rounded_image(p, tex, icon_r, 0.0),
                    None => {
                        let letter: String = app.name.chars().take(1).collect();
                        p.text(icon_r.center(), Align2::CENTER_CENTER, letter, semi(14.0), FG);
                    }
                }
                if app.muted {
                    p.line_segment([icon_r.left_bottom(), icon_r.right_top()], Stroke::new(2.5_f32, red));
                }
                if resp.clicked() {
                    mute = Some(i);
                }
                p.with_clip_rect(Rect::from_min_max(pos2(row.left() + 36.0, row.top()), pos2(row.left() + 166.0, row.bottom())))
                    .text(pos2(row.left() + 36.0, row.center().y), Align2::LEFT_CENTER, &app.name, semi(12.5), if app.active { FG } else { DIM });
                p.text(pos2(row.right() - 2.0, row.center().y), Align2::RIGHT_CENTER, format!("{:.0}%", app.level * 100.0), FontId::proportional(11.5), DIM);
                let mut v = app.level;
                let track = Rect::from_min_max(pos2(row.left() + 174.0, row.center().y - 6.0), pos2(row.right() - 44.0, row.center().y + 6.0));
                let tone = if app.active { accent() } else { Color32::from_rgb(150, 150, 166) };
                if slim_slider(ui, ui.id().with(("mix", i)), track, &mut v, tone, app.muted) {
                    set = Some((i, v));
                }
            }
        });
        if let Some((i, v)) = set {
            sys::set_app_volume(&self.mixer[i].path, v);
            self.mixer[i].level = v;
        }
        if let Some(i) = mute {
            let on = !self.mixer[i].muted;
            sys::set_app_mute(&self.mixer[i].path, on);
            self.mixer[i].muted = on;
        }
    }

    fn ui_weather(&mut self, ui: &mut egui::Ui) {
        let Some(w) = self.weather.clone() else {
            ui.add_space(30.0);
            let city_set = self.cfg.lat.is_some();
            let (head, sub) = weather::empty_text(city_set, self.weather_err.as_deref());
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(head).font(semi(16.5)));
                if !sub.is_empty() {
                    ui.label(RichText::new(sub).color(DIM));
                }
                if !city_set {
                    ui.add_space(6.0);
                    if jelly(ui, "Открыть настройки").clicked() {
                        self.tab = Tab::Settings;
                    }
                }
            });
            return;
        };
        let (icon, text) = weather::describe(w.code);
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{icon} {}", weather::deg(w.temp))).font(light(46.0)));
            ui.add_space(10.0);
            ui.vertical(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new(format!("{text} · {}", w.city)).font(semi(15.0)));
                ui.label(RichText::new(format!("Ощущается {} · влажность {:.0}% · ветер {:.1} м/с", weather::deg(w.feels), w.humidity, w.wind)).color(DIM));
            });
            if !w.sunrise.is_empty() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.vertical(|ui| {
                        ui.add_space(10.0);
                        ui.label(RichText::new(format!("Восход  {}", w.sunrise)).color(DIM).size(12.5));
                        ui.label(RichText::new(format!("Закат    {}", w.sunset)).color(DIM).size(12.5));
                    });
                });
            }
        });
        ui.add_space(6.0);

        // что надеть и взять: до двух подсказок по ближайшим 12 часам
        let tips = weather::advice_tips(w.feels, w.wind, &w.hours);
        if !tips.is_empty() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for (kind, tip) in &tips {
                    // значок и цвет по виду подсказки (Segoe MDL2 Assets)
                    let (glyph, tint) = match kind {
                        // те же знаки, что на плитках дней (weather::describe), — они точно есть в шрифте
                        weather::Tip::Rain => ("☂", Color32::from_rgb(0x8F, 0xCB, 0xFF)),
                        weather::Tip::Snow => ("❄", Color32::from_rgb(0xA8, 0xD8, 0xFF)),
                        weather::Tip::Cold => ("❄", Color32::from_rgb(0xA8, 0xD8, 0xFF)),
                        weather::Tip::Hot => ("☀", Color32::from_rgb(0xFF, 0xB3, 0x6B)),
                        weather::Tip::Wind => ("≋", Color32::from_rgb(0xC8, 0xD4, 0xE0)),
                    };
                    let font = FontId::proportional(12.5);
                    let tw = ui.painter().layout_no_wrap(tip.clone(), font.clone(), FG).size().x;
                    let (r, _) = ui.allocate_exact_size(vec2(tw + 38.0, 26.0), Sense::hover());
                    let p = ui.painter();
                    p.rect_filled(r, Rounding::same(13.0), tint.gamma_multiply(0.16));
                    p.rect_stroke(r, Rounding::same(13.0), Stroke::new(1.0_f32, tint.gamma_multiply(0.45)));
                    p.text(pos2(r.left() + 16.0, r.center().y), Align2::CENTER_CENTER, glyph, FontId::proportional(13.0), tint);
                    p.text(pos2(r.left() + 28.0, r.center().y), Align2::LEFT_CENTER, tip, font, FG);
                }
            });
            ui.add_space(6.0);
        }

        // ближайшие сутки: плавная кривая температуры, под ней — вероятность осадков
        if w.hours.len() >= 4 {
            let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), 132.0), Sense::hover());
            let p = ui.painter().with_clip_rect(area);
            glass_chip(&p, area, 16.0, 0.4);
            let n = w.hours.len();
            let (lo, hi) = w.hours.iter().fold((f64::MAX, f64::MIN), |(lo, hi), h| (lo.min(h.temp), hi.max(h.temp)));
            let span = (hi - lo).max(2.0);
            let wet = w.hours.iter().map(|h| h.rain).fold(0.0, f64::max);
            // подпись «осадки до …» стоит сверху: кривую опускаем, чтобы метки температуры её не задевали
            let (top, bottom) = (area.top() + if wet >= 15.0 { 48.0 } else { 34.0 }, area.bottom() - 40.0);
            let px = |i: f32| area.left() + 26.0 + (area.width() - 52.0) * i / (n - 1) as f32;
            let py = |t: f64| bottom - (bottom - top) * ((t - lo) / span) as f32;
            // сглаживание: между соседними часами — плавный переход
            let temp_at = |x: f32| -> f64 {
                let i = (x.floor() as usize).min(n - 2);
                let f = (x - i as f32) as f64;
                let k = f * f * (3.0 - 2.0 * f);
                w.hours[i].temp * (1.0 - k) + w.hours[i + 1].temp * k
            };
            let steps = (n - 1) * 8;
            let line: Vec<Pos2> = (0..=steps).map(|s| { let x = s as f32 / 8.0; pos2(px(x), py(temp_at(x))) }).collect();
            let c = accent();
            let mut mesh = egui::Mesh::default();
            for (k, pt) in line.iter().enumerate() {
                mesh.colored_vertex(*pt, Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 80));
                mesh.colored_vertex(pos2(pt.x, area.bottom() - 26.0), Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 0));
                if k > 0 {
                    let i = (k * 2) as u32;
                    mesh.add_triangle(i - 2, i - 1, i);
                    mesh.add_triangle(i - 1, i, i + 1);
                }
            }
            p.add(egui::Shape::mesh(mesh));
            p.add(egui::Shape::line(line, Stroke::new(2.2_f32, c)));
            for (i, h) in w.hours.iter().enumerate() {
                let x = px(i as f32);
                // осадки: столбик тем выше и ярче, чем вероятнее
                if h.rain >= 15.0 {
                    let bh = 3.0 + 13.0 * (h.rain / 100.0) as f32;
                    let r = Rect::from_min_max(pos2(x - 4.0, area.bottom() - 24.0 - bh), pos2(x + 4.0, area.bottom() - 24.0));
                    p.rect_filled(r, Rounding::same(2.0), Color32::from_rgba_unmultiplied(0x6E, 0xB8, 0xFF, (70.0 + 150.0 * (h.rain / 100.0)) as u8));
                }
                // подписи — каждые три часа, чтобы не слипались
                if i % 3 == 0 {
                    let now = i == 0;
                    p.circle_filled(pos2(x, py(h.temp)), if now { 4.5 } else { 3.0 }, if now { FG } else { c });
                    p.text(pos2(x, py(h.temp) - 9.0), Align2::CENTER_BOTTOM, weather::deg(h.temp), semi(12.5), FG);
                    p.text(pos2(x, area.bottom() - 6.0), Align2::CENTER_BOTTOM, if now { "сейчас".to_string() } else { format!("{:02}:00", h.hour) }, FontId::proportional(11.0), if now { FG } else { DIM });
                }
            }
            if wet >= 15.0 {
                p.text(area.left_top() + vec2(12.0, 8.0), Align2::LEFT_TOP, format!("осадки до {wet:.0}%"), FontId::proportional(11.0), Color32::from_rgb(0x8F, 0xCB, 0xFF));
            }
            ui.add_space(6.0);
        }

        if !w.days.is_empty() {
            // полоски диапазона считаются от самой холодной ночи до самого тёплого дня всей недели
            let lo = w.days.iter().map(|d| d.min).fold(f64::MAX, f64::min);
            let hi = w.days.iter().map(|d| d.max).fold(f64::MIN, f64::max).max(lo + 1.0);
            let now_t = w.temp;
            ui.columns(w.days.len(), |c| {
                for (i, d) in w.days.iter().enumerate() {
                    let (r, _) = c[i].allocate_exact_size(vec2(c[i].available_width(), 74.0), Sense::hover());
                    let p = c[i].painter();
                    glass_chip(p, r, 12.0, if i == 0 { 0.75 } else { 0.4 });
                    p.text(r.left_top() + vec2(12.0, 9.0), Align2::LEFT_TOP, if i == 0 { "Сегодня" } else { d.label.as_str() }, semi(12.0), if i == 0 { FG } else { DIM });
                    let tc = weather::tint(d.code);
                    let icon_c = Color32::from_rgb(tc[0], tc[1], tc[2]);
                    let ic = r.right_top() + vec2(-20.0, 17.0);
                    p.circle_filled(ic, 13.0, Color32::from_rgba_unmultiplied(tc[0], tc[1], tc[2], 12));
                    p.text(ic, Align2::CENTER_CENTER, weather::describe(d.code).0, FontId::proportional(16.0), icon_c);
                    // ряд «мин ─── макс»
                    let y = r.bottom() - 17.0;
                    let lo_g = p.text(pos2(r.left() + 12.0, y), Align2::LEFT_CENTER, weather::deg(d.min), FontId::proportional(12.0), DIM);
                    let hi_g = p.text(pos2(r.right() - 12.0, y), Align2::RIGHT_CENTER, weather::deg(d.max), semi(12.5), FG);
                    let track = Rect::from_min_max(pos2(lo_g.right() + 7.0, y - 2.0), pos2(hi_g.left() - 7.0, y + 2.0));
                    if track.width() > 8.0 {
                        p.rect_filled(track, Rounding::same(2.0), rgba(255, 255, 255, 20));
                        let fx = |t: f64| track.left() + track.width() * ((t - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
                        let (a, b) = (fx(d.min), fx(d.max).max(fx(d.min) + 4.0));
                        // градиент холод→тепло мелкими шагами
                        let steps = 10;
                        for s in 0..steps {
                            let x0 = a + (b - a) * s as f32 / steps as f32;
                            let x1 = a + (b - a) * (s + 1) as f32 / steps as f32;
                            let t = d.min + (d.max - d.min) * (s as f64 + 0.5) / steps as f64;
                            let c = weather::temp_rgb(t);
                            let round = if s == 0 || s == steps - 1 { 2.0 } else { 0.0 };
                            p.rect_filled(Rect::from_min_max(pos2(x0, track.top()), pos2(x1 + 0.5, track.bottom())), Rounding::same(round), Color32::from_rgb(c[0], c[1], c[2]));
                        }
                        if i == 0 {
                            // сейчас: белая точка с тёмной обводкой
                            let x = fx(now_t);
                            p.circle_filled(pos2(x, y), 4.5, BG);
                            p.circle_filled(pos2(x, y), 3.2, Color32::WHITE);
                        }
                    }
                }
            });
        }
        self.ui_sky_card(ui, &w);
    }

    /// Под погодой: фаза Луны и дуга солнца от восхода к закату — считается на месте, без сети.
    fn ui_sky_card(&self, ui: &mut egui::Ui, w: &weather::Now) {
        use chrono::Timelike;
        ui.add_space(8.0);
        let (card, _) = ui.allocate_exact_size(vec2(ui.available_width(), 66.0), Sense::hover());
        let p = ui.painter().clone();
        glass_chip(&p, card, 16.0, 0.45);
        // время города (восход и закат даны в его поясе)
        let city = chrono::Utc::now().naive_utc() + chrono::Duration::seconds(w.utc_offset as i64);
        let date = if test_mode() {
            std::env::var("OSTROVOK_MOON_DATE").ok().and_then(|s| chrono::NaiveDate::parse_from_str(&s, "%Y-%m-%d").ok()).unwrap_or(city.date())
        } else {
            city.date()
        };
        // ── Луна
        let (illum, name, waxing, to_full) = moon_phase(moon_age(date));
        let mc = pos2(card.left() + 36.0, card.center().y);
        moon_disc(&p, mc, 19.0, illum, waxing);
        p.text(pos2(card.left() + 68.0, card.top() + 15.0), Align2::LEFT_TOP, name, semi(13.0), FG);
        let sub = match to_full {
            0 => format!("освещена {:.0}% · сегодня полнолуние", illum * 100.0),
            n => format!("освещена {:.0}% · полнолуние через {}", illum * 100.0, count_ru(n as usize, ["день", "дня", "дней"])),
        };
        p.text(pos2(card.left() + 68.0, card.top() + 35.0), Align2::LEFT_TOP, sub, FontId::proportional(11.5), DIM);
        // ── солнце: дуга от восхода к закату и точка «сейчас»
        let hm = |s: &str| s.split_once(':').and_then(|(h, m)| Some(h.trim().parse::<f32>().ok()? + m.trim().parse::<f32>().ok()? / 60.0));
        let (Some(rise), Some(set)) = (hm(&w.sunrise), hm(&w.sunset)) else {
            // полярный день или ночь: времени восхода/заката нет
            p.text(pos2(card.right() - 24.0, card.center().y), Align2::RIGHT_CENTER, "солнце сегодня не восходит или не заходит", FontId::proportional(11.5), DIM);
            return;
        };
        let arc = Rect::from_min_max(pos2(card.center().x + 40.0, card.top() + 12.0), pos2(card.right() - 20.0, card.bottom() - 18.0));
        let base = arc.bottom();
        let pt = |f: f32| pos2(arc.left() + arc.width() * f, base - arc.height() * (std::f32::consts::PI * f).sin());
        let mut line = Vec::new();
        for i in 0..=40 {
            line.push(pt(i as f32 / 40.0));
        }
        p.line_segment([pos2(arc.left() - 8.0, base), pos2(arc.right() + 8.0, base)], Stroke::new(1.0_f32, rgba(255, 255, 255, 30)));
        p.add(egui::Shape::dashed_line(&line, Stroke::new(1.2_f32, rgba(255, 210, 120, 110)), 4.0, 3.0));
        let now_h = city.hour() as f32 + city.minute() as f32 / 60.0;
        let f = (now_h - rise) / (set - rise).max(0.1);
        if (0.0..=1.0).contains(&f) {
            let sun = pt(f);
            p.circle_filled(sun, 9.0, rgba(255, 200, 90, 50));
            p.circle_filled(sun, 5.0, Color32::from_rgb(255, 204, 92));
        }
        p.text(pos2(arc.left(), base + 9.0), Align2::CENTER_CENTER, &w.sunrise, FontId::proportional(10.5), DIM);
        p.text(pos2(arc.right(), base + 9.0), Align2::CENTER_CENTER, &w.sunset, FontId::proportional(10.5), DIM);
        let len = day_length(rise, set);
        p.text(pos2(arc.center().x, base + 9.0), Align2::CENTER_CENTER, format!("день {} ч {} мин", len / 60, len % 60), FontId::proportional(11.0), FG);
    }

    /// Инструменты → Таймер: циферблат с остатком, готовые отрезки, своё время и секундомер.
    fn ui_timer(&mut self, ui: &mut egui::Ui) {
        let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), 156.0), Sense::hover());
        let p = ui.painter().clone();
        let c = pos2(area.left() + 80.0, area.center().y);
        let (secs, frac, running, paused) = timer_view(self.timer_end, self.timer_paused, self.timer_total, Instant::now());
        let text = fmt_secs(secs);
        // на паузе циферблат застывает, но не гаснет: кольцо и деления — приглушённым цветом
        let live = running || paused;
        let tone = match self.focus {
            Some((true, _)) => Color32::from_rgb(0x7B, 0xE0, 0x9A), // перерыв — зелёный
            _ => Color32::from_rgb(0xFF, 0xB3, 0x40),
        };
        let tone = if paused { mix(tone, Color32::from_rgb(120, 120, 134), 0.55) } else { tone };
        // циферблат: 60 делений, каждое пятое длиннее; в работе оставшаяся часть подсвечена
        for i in 0..60 {
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * i as f32 / 60.0;
            let dir = vec2(a.cos(), a.sin());
            let major = i % 5 == 0;
            let (r0, r1) = if major { (50.0, 57.0) } else { (53.5, 57.0) };
            let lit = live && (i as f32) < frac * 60.0;
            let col = if lit { mix(tone, BG, if major { 0.1 } else { 0.45 }) } else { rgba(255, 255, 255, if major { 70 } else { 28 }) };
            p.line_segment([c + dir * r0, c + dir * r1], Stroke::new(if major { 1.6_f32 } else { 1.0 }, col));
        }
        ring(&p, c, 66.0, 7.0, frac, tone);
        if live && frac > 0.003 {
            // светящаяся точка на конце дуги
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * frac;
            let tip = c + vec2(a.cos(), a.sin()) * 66.0;
            p.circle_filled(tip, 9.0, Color32::from_rgba_unmultiplied(tone.r(), tone.g(), tone.b(), 50));
            p.circle_filled(tip, 5.0, Color32::WHITE);
        }
        p.text(c - vec2(0.0, 5.0), Align2::CENTER_CENTER, text, light(30.0), if live { FG } else { DIM });
        let sub = match (live, self.focus) {
            _ if paused => "\u{E769} пауза".to_string(),
            (true, Some((true, _))) => "перерыв".to_string(),
            (true, Some((false, _))) => "фокус".to_string(),
            (true, None) => {
                let left = self.timer_end.map(|e| e.saturating_duration_since(Instant::now())).unwrap_or_default();
                let at = chrono::Local::now() + chrono::Duration::from_std(left).unwrap_or_default();
                format!("до {}", at.format("%H:%M"))
            }
            _ => "таймер".to_string(),
        };
        p.text(c + vec2(0.0, 24.0), Align2::CENTER_CENTER, sub, FontId::proportional(11.5), DIM);

        let mut side = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(area.left() + 178.0, area.top() + 4.0), area.max)));
        let ui = &mut side;
        let mut start = None;
        ui.columns(5, |c| {
            for (i, m) in [1.0, 5.0, 10.0, 25.0, 60.0].into_iter().enumerate() {
                let w = c[i].available_width();
                if jelly_sized(&mut c[i], format!("{} мин", fmt_num(m)), Some(vec2(w, 38.0))).clicked() {
                    start = Some(m);
                }
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Своё время, мин").color(DIM));
            ui.add(egui::TextEdit::singleline(&mut self.timer_custom).margin(egui::Margin::symmetric(9.0, 7.0)).desired_width(60.0));
            if jelly(ui, "Старт").clicked() {
                start = self.timer_custom.trim().replace(',', ".").parse::<f64>().ok();
            }
            if live && jelly(ui, "Стоп").clicked() {
                // остановили посреди рабочего круга — прошедшее время засчитываем, если набралось от 5 минут
                if let Some((false, _)) = self.focus {
                    // остаток берём и у идущего таймера, и у стоящего на паузе
                    let left = timer_view(self.timer_end, self.timer_paused, self.timer_total, Instant::now()).0 as f32;
                    let done = ((self.timer_total - left) / 60.0).floor() as u32;
                    if done >= 5 {
                        self.log_focus(done);
                    }
                }
                self.timer_end = None;
                self.timer_paused = None;
                self.focus = None;
            }
            // пауза: остаток запоминаем, таймер останавливаем — пилюля и капля перестают тикать
            if live && jelly(ui, if paused { "\u{E768}" } else { "\u{E769}" }).on_hover_text(if paused { "Продолжить" } else { "Пауза" }).clicked() {
                if let Some(left) = self.timer_paused.take() {
                    self.timer_end = Some(Instant::now() + left);
                } else if let Some(end) = self.timer_end.take() {
                    self.timer_paused = Some(end.saturating_duration_since(Instant::now()));
                }
            }
            let label = match self.focus {
                Some((_, rounds)) => format!("Фокус: круг {}", rounds + 1),
                None => "Фокус 25/5".to_string(),
            };
            if jelly(ui, label).on_hover_text("25 минут работы, 5 минут перерыва — по кругу, пока не остановите").clicked() {
                if self.focus.take().is_some() {
                    self.timer_end = None;
                    self.timer_paused = None;
                } else {
                    self.focus = Some((false, 0));
                    self.start_timer(25.0);
                }
            }
        });
        if let Some(m) = start {
            // неверное время не должно сбрасывать идущий фокус или паузу
            if timer_minutes_ok(m) {
                self.focus = None;
                self.start_timer(m);
            } else {
                self.notify("Время таймера — от доли минуты до суток");
            }
        }
        ui.add_space(6.0);
        let (card, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
        glass_chip(ui.painter(), card, 14.0, 0.5);
        let run = self.sw_acc + self.sw_start.map_or(Duration::ZERO, |t| t.elapsed());
        let secs = run.as_secs();
        ui.painter().text(pos2(card.left() + 16.0, card.center().y), Align2::LEFT_CENTER, "Секундомер", FontId::proportional(13.0), DIM);
        ui.painter().text(pos2(card.left() + 108.0, card.center().y), Align2::LEFT_CENTER, format!("{:02}:{:02}.{}", secs / 60, secs % 60, run.subsec_millis() / 100), semi(18.0), FG);
        let mut buttons = ui.new_child(egui::UiBuilder::new().max_rect(card.shrink2(vec2(8.0, 7.0))).layout(egui::Layout::right_to_left(egui::Align::Center)));
        if jelly(&mut buttons, "Сброс").clicked() {
            self.sw_start = None;
            self.sw_acc = Duration::ZERO;
        }
        if jelly(&mut buttons, if self.sw_start.is_some() { "Пауза" } else { "Пуск" }).clicked() {
            match self.sw_start.take() {
                Some(t) => self.sw_acc += t.elapsed(),
                None => self.sw_start = Some(Instant::now()),
            }
        }
    }

    /// Под календарём: мировые часы — города с местным временем и небом по их времени суток.
    fn ui_world(&mut self, ui: &mut egui::Ui) {
        use chrono::Timelike;
        ui.add_space(8.0);
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::hover());
        let utc = chrono::Utc::now().naive_utc();
        let here = chrono::Local::now().naive_local();
        let mut x = row.left();
        let mut remove = None;
        // таблетки делят ширину поровну (кнопке «+ город» оставляем место, пока городов меньше трёх)
        let n = self.cfg.world.len().max(1) as f32;
        let reserve = if self.cfg.world.len() < WORLD_MAX { 118.0 } else { 0.0 };
        let w = ((row.width() - reserve - 8.0 * (n - 1.0)) / n).clamp(120.0, 210.0);
        for (i, name) in self.cfg.world.iter().enumerate() {
            let Some(city) = find_city(name) else { continue };
            let there = utc + chrono::Duration::minutes(city_offset(city, utc) as i64);
            let label = world_label(here, there);
            let r = Rect::from_min_size(pos2(x, row.top()), vec2(w, row.height()));
            let resp = ui.interact(r, ui.id().with(("world", i)), Sense::click()).on_hover_text("Правый щелчок — убрать город");
            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
            let p = ui.painter();
            glass_chip(p, r, 14.0, 0.4 + 0.4 * hov);
            // небо по местному часу города (восход и закат условно 7:00 и 19:00)
            let h = there.hour() as f32 + there.minute() as f32 / 60.0;
            let (top, low) = sky_colors(day_phase(h, 7.0, 19.0));
            gradient_rrect(p, r.shrink(1.0), 13.0, |q| mix_a(top, low, ((q.y - r.top()) / r.height()).clamp(0.0, 1.0)));
            p.text(pos2(r.left() + 12.0, r.top() + 8.0), Align2::LEFT_TOP, city.show, semi(12.0), FG);
            p.text(pos2(r.left() + 12.0, r.bottom() - 8.0), Align2::LEFT_BOTTOM, label, FontId::proportional(11.0), DIM);
            p.text(pos2(r.right() - 12.0, r.center().y), Align2::RIGHT_CENTER, there.format("%H:%M").to_string(), light(20.0), FG);
            if resp.secondary_clicked() {
                remove = Some(i);
            }
            x += w + 8.0;
        }
        if let Some(i) = remove {
            self.cfg.world.remove(i);
            self.cfg.save();
        }
        // справа — добавить город: кнопка или строка ввода
        let rest = Rect::from_min_max(pos2(x, row.top()), row.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rest).layout(egui::Layout::left_to_right(egui::Align::Center)));
        match &mut self.world_edit {
            Some(text) => {
                let r = child.add(egui::TextEdit::singleline(text).hint_text(RichText::new("Город, Enter").color(DIM)).margin(egui::Margin::symmetric(9.0, 7.0)).desired_width((rest.width() - 10.0).clamp(80.0, 170.0)));
                if self.world_focus {
                    r.request_focus();
                    self.world_focus = false;
                }
                let enter = r.lost_focus() && child.input(|i| i.key_pressed(egui::Key::Enter));
                let esc = child.input(|i| i.key_pressed(egui::Key::Escape));
                if enter {
                    match find_city(text) {
                        Some(c) if self.cfg.world.iter().any(|w| w == c.show) => self.world_edit = None,
                        Some(_) if self.cfg.world.len() >= WORLD_MAX => {
                            self.notify("Не больше трёх городов — уберите один правым щелчком");
                            self.world_edit = None;
                        }
                        Some(c) => {
                            self.cfg.world.push(c.show.to_string());
                            self.cfg.save();
                            self.world_edit = None;
                        }
                        None => {
                            self.notify("Не знаю такой город — попробуйте, например, «Лондон» или «Токио»");
                            // оставляем поле в фокусе, чтобы поправить опечатку
                            self.world_focus = true;
                        }
                    }
                } else if esc || (r.lost_focus() && text.trim().is_empty()) {
                    self.world_edit = None;
                }
            }
            None if self.cfg.world.len() >= WORLD_MAX => {}
            None => {
                let hint = if self.cfg.world.is_empty() { "+ мировые часы" } else { "+ город" };
                if jelly(&mut child, hint).on_hover_text("Добавить город: время, разница и день или ночь там").clicked() {
                    self.world_edit = Some(String::new());
                    self.world_focus = true;
                }
            }
        }
    }

    /// Под таймером: минуты «Фокуса» за неделю столбиками и серия дней подряд.
    fn ui_focus_week(&mut self, ui: &mut egui::Ui) {
        use chrono::Datelike;
        ui.add_space(8.0);
        let (card, _) = ui.allocate_exact_size(vec2(ui.available_width(), 78.0), Sense::hover());
        let p = ui.painter().clone();
        glass_chip(&p, card, 16.0, 0.45);
        let today = chrono::Local::now().date_naive();
        let week = week_minutes(&self.cfg.focus_log, today);
        let streak = focus_streak(&self.cfg.focus_log, today);
        let tone = Color32::from_rgb(0xFF, 0xB3, 0x40);
        let total: u32 = week.iter().sum();
        if total == 0 {
            p.text(pos2(card.left() + 18.0, card.center().y - 9.0), Align2::LEFT_CENTER, "Неделя фокуса", semi(13.0), FG);
            p.text(pos2(card.left() + 18.0, card.center().y + 11.0), Align2::LEFT_CENTER, "Запустите «Фокус 25/5» — здесь появится ваша неделя и серия дней", FontId::proportional(11.5), DIM);
            return;
        }
        // слева — итоги
        p.text(pos2(card.left() + 18.0, card.top() + 16.0), Align2::LEFT_TOP, format!("Сегодня {}", fmt_minutes(week[6])), semi(13.5), FG);
        p.text(pos2(card.left() + 18.0, card.top() + 38.0), Align2::LEFT_TOP, format!("за неделю {}", fmt_minutes(total)), FontId::proportional(11.5), DIM);
        if streak > 1 {
            p.text(pos2(card.left() + 18.0, card.top() + 54.0), Align2::LEFT_TOP, format!("\u{E945} серия {} {}", streak, count_ru(streak as usize, ["день", "дня", "дней"]).split_once(' ').map_or("", |s| s.1)), FontId::proportional(11.5), tone);
        }
        // справа — семь столбиков, сегодня последним
        const DAYS: [&str; 7] = ["пн", "вт", "ср", "чт", "пт", "сб", "вс"];
        let area = Rect::from_min_max(pos2(card.left() + 210.0, card.top() + 10.0), pos2(card.right() - 16.0, card.bottom() - 20.0));
        let step = area.width() / 7.0;
        let bw = (step * 0.42).min(22.0);
        let top = (*week.iter().max().unwrap_or(&0)).max(25) as f32;
        for (i, m) in week.iter().enumerate() {
            let cx = area.left() + step * (i as f32 + 0.5);
            let day = today - chrono::Duration::days(6 - i as i64);
            let is_today = i == 6;
            if *m == 0 {
                p.circle_filled(pos2(cx, area.bottom() - 2.0), 2.0, rgba(255, 255, 255, 28));
            } else {
                let h = (area.height() * *m as f32 / top).max(4.0);
                let r = Rect::from_min_max(pos2(cx - bw / 2.0, area.bottom() - h), pos2(cx + bw / 2.0, area.bottom()));
                if is_today {
                    p.rect_filled(r.expand(3.0), Rounding::same(6.0), Color32::from_rgba_unmultiplied(tone.r(), tone.g(), tone.b(), 46));
                }
                p.rect_filled(r, Rounding::same(4.0), if is_today { tone } else { mix(tone, BG, 0.45) });
            }
            let hit = Rect::from_center_size(pos2(cx, area.center().y), vec2(step, area.height() + 20.0));
            ui.interact(hit, ui.id().with(("focus-day", i)), Sense::hover()).on_hover_text(format!("{} · {}", DAYS[day.weekday().num_days_from_monday() as usize], fmt_minutes(*m)));
            p.text(pos2(cx, card.bottom() - 10.0), Align2::CENTER_CENTER, DAYS[day.weekday().num_days_from_monday() as usize], FontId::proportional(10.5), if is_today { FG } else { DIM });
        }
    }

    /// Записи → Буфер: история скопированного; для ссылок, цветов, примеров и путей — готовое действие.
    fn ui_clip(&mut self, ui: &mut egui::Ui) {
        if self.clips.is_empty() && self.cfg.pins.is_empty() && self.clip_imgs.is_empty() {
            empty_state(ui, "\u{E77F}", "Буфер пуст", "Скопируйте текст или снимок — он появится здесь, а остров подскажет, что с ним сделать");
            return;
        }
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 100.0).max(120.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.clip_query)
                    .margin(egui::Margin::symmetric(9.0, 7.0))
                    .hint_text(RichText::new("Поиск по буферу. Щелчок по строке копирует её снова").color(DIM))
                    .desired_width(w),
            );
            if jelly(ui, "Очистить").on_hover_text("Убрать историю и снимки. Закреплённые записи останутся").clicked() {
                self.clips.clear();
                self.clip_imgs.clear();
            }
        });
        // ── быстрые преобразования последней текстовой записи: результат сразу в буфер
        // считаются каждый кадр, поэтому для огромных записей полосу не показываем
        if let Some(last) = self.clips.first().or(self.cfg.pins.first()).filter(|t| t.len() <= 20_000).cloned() {
            ui.add_space(4.0);
            let mut done = None;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(RichText::new("\u{E8AC}").size(12.0).color(accent()));
                ui.label(RichText::new("Последнее:").color(DIM).size(11.5));
                for (tx, name, tip) in ClipTx::ALL {
                    let out = clip_transform(tx, &last);
                    let same = out == last;
                    let sample: String = out.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(60).collect();
                    let b = ui.add_enabled(!same, egui::Button::new(RichText::new(name).size(11.5)).rounding(9.0).min_size(vec2(0.0, 22.0)));
                    if b.on_hover_text(format!("{tip}\n→ {sample}")).on_disabled_hover_text("Текст от этого не изменится").clicked() {
                        done = Some((out, name));
                    }
                }
            });
            if let Some((out, name)) = done {
                self.set_clipboard(&out);
                self.notify(&format!("«{name}» — готово, скопировано"));
            }
        }
        // ── снимки: полоса миниатюр. Щелчок — снова в буфер, правый щелчок — текст с картинки
        if !self.clip_imgs.is_empty() {
            ui.add_space(6.0);
            let (strip, _) = ui.allocate_exact_size(vec2(ui.available_width(), 64.0), Sense::hover());
            let (mut put, mut ocr, mut drop) = (None, None, None);
            let mut x = strip.left();
            for (i, im) in self.clip_imgs.iter().enumerate() {
                let sz = thumb_size(im.w, im.h, 54.0);
                if x + sz.x > strip.right() {
                    break;
                }
                let r = Rect::from_min_size(pos2(x, strip.top() + 2.0), sz);
                let resp = ui.interact(r, ui.id().with(("clip-img", im.hash)), Sense::click()).on_hover_text("Щелчок — снова в буфер · правый щелчок — текст с картинки");
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.12);
                let p = ui.painter();
                rounded_image(p, &im.tex, r, 9.0);
                p.rect_stroke(r.expand(1.5 * hov), Rounding::same(10.0), Stroke::new(1.0 + 1.0 * hov, mix(rgba(255, 255, 255, 40), accent(), hov)));
                if hov > 0.0 {
                    // размер снимка и крестик
                    let label = format!("{}×{}", im.w, im.h);
                    let lr = Rect::from_min_size(pos2(r.left() + 4.0, r.bottom() - 17.0), vec2(r.width() - 8.0, 14.0));
                    p.rect_filled(lr, Rounding::same(5.0), rgba(10, 10, 18, (170.0 * hov) as u8));
                    p.text(lr.center(), Align2::CENTER_CENTER, label, FontId::proportional(10.0), FG.gamma_multiply(hov));
                    let xr = Rect::from_center_size(pos2(r.right() - 9.0, r.top() + 9.0), vec2(16.0, 16.0));
                    let xb = ui.interact(xr, ui.id().with(("clip-img-x", im.hash)), Sense::click());
                    p.circle_filled(xr.center(), 8.0, rgba(10, 10, 18, (200.0 * hov) as u8));
                    p.text(xr.center(), Align2::CENTER_CENTER, "\u{E711}", FontId::proportional(8.0), FG.gamma_multiply(hov));
                    if xb.clicked() {
                        drop = Some(i);
                    }
                }
                if drop != Some(i) {
                    if resp.clicked() {
                        put = Some(i);
                    } else if resp.secondary_clicked() {
                        ocr = Some(i);
                    }
                }
                x += sz.x + 8.0;
            }
            if let Some(i) = drop {
                self.clip_imgs.remove(i);
            } else if let Some(i) = put {
                let im = &self.clip_imgs[i];
                let data = arboard::ImageData { width: im.w, height: im.h, bytes: std::borrow::Cow::Owned(im.px.clone()) };
                // свою запись в буфер не принимаем за новый снимок
                self.clip_skip = true;
                let ok = self.clipboard.as_mut().is_some_and(|cb| cb.set_image(data).is_ok());
                self.notify(if ok { "Снимок снова в буфере" } else { "Не удалось положить снимок в буфер" });
            } else if let Some(i) = ocr {
                let im = &self.clip_imgs[i];
                match sys::ocr(im.w, im.h, &im.px) {
                    Ok(t) if !t.trim().is_empty() => {
                        let n = t.chars().count();
                        self.set_clipboard(&t);
                        self.notify(&format!("Текст с картинки скопирован: {n} симв."));
                    }
                    Ok(_) => self.notify("Текст на картинке не найден"),
                    Err(e) => self.notify(&e),
                }
            }
        }
        let q = self.clip_query.trim().to_lowercase();
        // сначала закреплённые, потом история без повторов
        let rows: Vec<(bool, String)> = self
            .cfg
            .pins
            .iter()
            .map(|c| (true, c.clone()))
            .chain(self.clips.iter().filter(|c| !self.cfg.pins.contains(c)).map(|c| (false, c.clone())))
            .filter(|(_, c)| q.is_empty() || c.to_lowercase().contains(&q))
            .collect();
        let (mut pick, mut act, mut pin) = (None, None, None);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if rows.is_empty() {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("Ничего не найдено").color(DIM)));
            }
            for (i, (pinned, c)) in rows.iter().enumerate() {
                let hint = smart_hint(c);
                let preview: String = c.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(110).collect();
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
                glass_chip(ui.painter(), r, 11.0, 0.4 + 0.5 * hov);
                let (glyph, color) = match &hint {
                    Some(h) => (h.glyph, Color32::from_rgb(h.color[0], h.color[1], h.color[2])),
                    None => ("\u{E8C8}", DIM),
                };
                ui.painter().text(pos2(r.left() + 20.0, r.center().y), Align2::CENTER_CENTER, glyph, FontId::proportional(14.0), color);
                // справа: булавка и, если остров узнал содержимое, действие
                let pr = Rect::from_center_size(pos2(r.right() - 20.0, r.center().y), vec2(28.0, 26.0));
                let pb = ui.interact(pr, ui.id().with(("clip-pin", i)), Sense::click()).on_hover_text(if *pinned { "Открепить" } else { "Закрепить: запись останется после перезапуска" });
                let ph = ui.ctx().animate_bool_with_time(pb.id.with("hov"), pb.hovered(), 0.10);
                if *pinned || ph > 0.0 {
                    glass_chip(ui.painter(), pr, 9.0, 0.5 + 0.6 * ph);
                }
                ui.painter().text(pr.center(), Align2::CENTER_CENTER, if *pinned { "\u{E840}" } else { "\u{E718}" }, FontId::proportional(12.5), if *pinned { accent() } else if hov > 0.0 || ph > 0.0 { FG } else { mix(DIM, BG, 0.45) });
                let act_w = if hint.is_some() { 158.0 } else { 0.0 };
                let text_right = r.right() - 40.0 - if hint.is_some() { act_w + 6.0 } else { 0.0 };
                ui.painter()
                    .with_clip_rect(Rect::from_min_max(pos2(r.left() + 38.0, r.top()), pos2(text_right, r.bottom())))
                    .text(pos2(r.left() + 38.0, r.center().y), Align2::LEFT_CENTER, preview, FontId::proportional(13.0), FG);
                let mut used = false;
                if pb.clicked() {
                    pin = Some((c.clone(), !*pinned));
                    used = true;
                }
                if let Some(h) = hint {
                    let br = Rect::from_min_max(pos2(r.right() - 40.0 - act_w, r.top() + 4.0), pos2(r.right() - 40.0, r.bottom() - 4.0));
                    let b = ui.interact(br, ui.id().with(("clip-act", i)), Sense::click());
                    let bh = ui.ctx().animate_bool_with_time(b.id.with("hov"), b.hovered(), 0.10);
                    glass_chip(ui.painter(), br, 9.0, 0.6 + 0.6 * bh);
                    ui.painter().with_clip_rect(br).text(br.center(), Align2::CENTER_CENTER, &h.label, semi(12.0), accent());
                    if b.clicked() {
                        act = Some(h.act);
                        used = true;
                    }
                }
                if !used && resp.clicked() {
                    pick = Some(c.clone());
                }
            }
        });
        if let Some((text, on)) = pin {
            self.cfg.pins.retain(|p| p != &text);
            if on {
                self.cfg.pins.insert(0, text);
                self.cfg.pins.truncate(20);
            } else if !self.clips.contains(&text) {
                // открепили — запись возвращается в обычную историю
                self.clips.insert(0, text);
            }
            self.cfg.save();
        } else if let Some(a) = act {
            self.do_hint(a);
        } else if let Some(text) = pick {
            self.set_clipboard(&text);
            self.notify("Скопировано в буфер");
        }
    }

    fn log_focus(&mut self, min: u32) {
        add_focus(&mut self.cfg.focus_log, chrono::Local::now().date_naive(), min);
        self.cfg.save();
    }

    /// Кладёт картинку в историю буфера: повтор переезжает в начало, храним не больше восьми.
    fn push_clip_img(&mut self, ctx: &egui::Context, w: usize, h: usize, px: Vec<u8>) {
        let hash = img_hash(&px);
        if let Some(i) = self.clip_imgs.iter().position(|c| c.hash == hash) {
            let it = self.clip_imgs.remove(i);
            self.clip_imgs.insert(0, it);
            return;
        }
        let Some((tw, th_h)) = clip_thumb_size(w, h) else { return };
        let Some(full) = image::RgbaImage::from_raw(w as u32, h as u32, px.clone()) else { return };
        let th = image::imageops::thumbnail(&full, tw, th_h);
        let tex = ctx.load_texture(format!("clip{hash:x}"), egui::ColorImage::from_rgba_unmultiplied([th.width() as usize, th.height() as usize], th.as_raw()), egui::TextureOptions::LINEAR);
        self.clip_imgs.insert(0, ClipImg { w, h, px, hash, tex });
        self.clip_imgs.truncate(8);
    }

    fn push_inbox(&mut self, n: live::Notice, color: [u8; 3]) {
        self.inbox.insert(0, InboxItem { app: n.app, title: n.title, body: n.body, color, at: chrono::Local::now() });
        self.inbox.truncate(50);
        self.inbox_unread = (self.inbox_unread + 1).min(self.inbox.len());
    }

    /// Записи → Уведомления: что пришло в мессенджеры, пока вы были заняты.
    fn ui_inbox(&mut self, ui: &mut egui::Ui) {
        self.inbox_unread = 0;
        if self.inbox.is_empty() {
            let hint = if self.cfg.mirror {
                "Здесь соберутся сообщения из Telegram, Discord и других мессенджеров"
            } else {
                "Включите «Уведомления мессенджеров» в настройках, и они будут собираться здесь"
            };
            empty_state(ui, "\u{EA8F}", "Новых сообщений нет", hint);
            return;
        }
        // сводка: сколько и откуда; фишки мессенджеров — фильтр
        let by_app = inbox_apps(&self.inbox);
        if self.inbox_filter.as_ref().is_some_and(|f| !by_app.iter().any(|a| &a.0 == f)) {
            self.inbox_filter = None; // сообщений этого мессенджера не осталось
        }
        let visible = inbox_visible(&self.inbox, self.inbox_filter.as_deref());
        let (mut clear, mut toggle) = (false, None);
        ui.horizontal(|ui| {
            ui.add_space(4.0);
            ui.label(RichText::new(count_ru(visible.len(), ["сообщение", "сообщения", "сообщений"])).font(semi(14.0)));
            ui.add_space(4.0);
            for (k, (app, n, c)) in by_app.iter().take(4).enumerate() {
                let color = Color32::from_rgb(c[0], c[1], c[2]);
                let on = self.inbox_filter.as_deref() == Some(app.as_str());
                let label = format!("{app} {n}");
                let tw = ui.painter().layout_no_wrap(label.clone(), FontId::proportional(12.0), FG).size().x;
                let (r, resp) = ui.allocate_exact_size(vec2(tw + 28.0, 24.0), Sense::click());
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
                let t = ui.ctx().animate_bool_with_time(ui.id().with(("inbox-chip", k, app.as_str())), on, 0.15);
                let p = ui.painter();
                // активная фишка заливается цветом мессенджера
                p.rect_filled(r, Rounding::same(12.0), mix(rgba(255, 255, 255, (10.0 + 14.0 * hov) as u8), color.gamma_multiply(0.35), t));
                p.rect_stroke(r, Rounding::same(12.0), Stroke::new(1.0_f32, mix(rgba(255, 255, 255, 22), color, t)));
                p.circle_filled(pos2(r.left() + 12.0, r.center().y), 3.5, color);
                p.text(pos2(r.left() + 20.0, r.center().y), Align2::LEFT_CENTER, label, FontId::proportional(12.0), mix(DIM, FG, t.max(hov)));
                let tip = if on { "Показать все сообщения".to_string() } else { format!("Только {app}") };
                if resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    toggle = Some(if on { None } else { Some(app.clone()) });
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let tip = if self.inbox_filter.is_some() { "Убрать показанные сообщения этого мессенджера" } else { "Убрать все сообщения из списка" };
                if jelly(ui, "Очистить").on_hover_text(tip).clicked() {
                    clear = true;
                }
            });
        });
        if let Some(f) = toggle {
            self.inbox_filter = f;
        }
        ui.add_space(4.0);
        let now = chrono::Local::now();
        let mut pick: Option<usize> = None;
        let mut drop: Option<usize> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            for &i in &visible {
                let it = &self.inbox[i];
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width() - 6.0, 50.0), Sense::click());
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
                let p = ui.painter();
                glass_chip(p, r, 14.0, 0.4 + 0.5 * hov);
                // цветная полоска слева — по мессенджеру
                let c = Color32::from_rgb(it.color[0], it.color[1], it.color[2]);
                let letter: String = it.title.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "·".into());
                let top = [it.color[0].saturating_add(40), it.color[1].saturating_add(40), it.color[2].saturating_add(40)];
                icon_disc(p, pos2(r.left() + 27.0, r.center().y), 16.0, top, it.color, &letter, 14.0);
                let x = r.left() + 54.0;
                let when = ago_ru((now - it.at).num_minutes(), &it.at.format("%H:%M").to_string());
                let when_w = p.layout_no_wrap(when.clone(), FontId::proportional(11.5), DIM).size().x;
                let close = Rect::from_center_size(pos2(r.right() - 20.0, r.center().y), vec2(26.0, 26.0));
                let right = r.right() - 14.0 - if hov > 0.0 { 30.0 } else { 0.0 };
                let clip = p.with_clip_rect(Rect::from_min_max(pos2(x, r.top()), pos2(right - when_w - 10.0, r.bottom())));
                let head = clip.text(pos2(x, r.top() + 9.0), Align2::LEFT_TOP, &it.title, semi(13.0), FG);
                clip.text(pos2(head.right() + 8.0, r.top() + 10.5), Align2::LEFT_TOP, &it.app, FontId::proportional(11.0), mix(c, DIM, 0.35));
                let body: String = it.body.split_whitespace().collect::<Vec<_>>().join(" ");
                let body = if body.is_empty() { "без текста".to_string() } else { body };
                p.with_clip_rect(Rect::from_min_max(pos2(x, r.top()), pos2(right, r.bottom())))
                    .text(pos2(x, r.top() + 28.0), Align2::LEFT_TOP, body, FontId::proportional(12.5), DIM);
                p.text(pos2(right, r.top() + 10.0), Align2::RIGHT_TOP, when, FontId::proportional(11.5), mix(DIM, BG, 0.25));
                let mut used = false;
                if hov > 0.0 {
                    let b = ui.interact(close, ui.id().with(("inbox-x", i)), Sense::click()).on_hover_text("Убрать");
                    let bh = ui.ctx().animate_bool_with_time(b.id.with("hov"), b.hovered(), 0.10);
                    glass_chip(ui.painter(), close, 9.0, (0.3 + 0.7 * bh) * hov);
                    ui.painter().text(close.center(), Align2::CENTER_CENTER, "\u{E711}", FontId::proportional(10.5), mix(DIM, FG, bh).gamma_multiply(hov));
                    if b.clicked() {
                        drop = Some(i);
                        used = true;
                    }
                }
                if !used && resp.on_hover_text("Щелчок копирует текст сообщения").clicked() {
                    pick = Some(i);
                }
            }
        });
        if clear {
            // при фильтре убираем только показанные; фильтр сам сбросится в следующем кадре
            match self.inbox_filter.take() {
                Some(f) => self.inbox.retain(|it| it.app != f),
                None => self.inbox.clear(),
            }
        } else if let Some(i) = drop {
            self.inbox.remove(i);
        } else if let Some(i) = pick {
            let text = if self.inbox[i].body.is_empty() { self.inbox[i].title.clone() } else { self.inbox[i].body.clone() };
            self.set_clipboard(&text);
            self.notify("Текст сообщения скопирован");
        }
    }

    /// Показывает карточку снимка: миниатюра и что с ним можно сделать.
    fn show_shot(&mut self, ctx: &egui::Context, w: usize, h: usize, px: Vec<u8>, path: String) {
        // пустой снимок 0×N: миниатюра из него не строится, а деление на 0 даёт бесконечность
        if w == 0 || h == 0 {
            return;
        }
        // миниатюра — не больше 640 точек по длинной стороне
        let k = (640.0 / w.max(h) as f32).min(1.0);
        let (tw, th) = (((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1));
        let Some(img) = image::RgbaImage::from_raw(w as u32, h as u32, px.clone()) else { return };
        let small = image::imageops::thumbnail(&img, tw, th);
        let tex = ctx.load_texture("shot-card", egui::ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], small.as_raw()), Default::default());
        self.shot_card = Some(ShotCard { path, tex, size: (w, h), rgba: Arc::new(px), until: Instant::now() + Duration::from_secs(8) });
        self.shot_marks.clear();
        self.shot_drag = None;
        self.mode = Mode::Panel;
        self.tab = Tab::Shot;
        self.sticky = true;
    }

    /// Карточка снимка экрана.
    fn ui_shot(&mut self, ui: &mut egui::Ui) {
        let Some(card) = &self.shot_card else {
            empty_state(ui, "\u{E722}", "Снимков пока нет", "Сделайте снимок экрана — здесь появятся действия с ним");
            return;
        };
        let (path, size, tex, rgba) = (card.path.clone(), card.size, card.tex.clone(), card.rgba.clone());
        let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 196.0), Sense::hover());
        // слева миниатюра: вписана в рамку с сохранением пропорций
        let frame = Rect::from_min_size(row.min, vec2(330.0, row.height()));
        let p = ui.painter().clone();
        glass_chip(&p, frame, 16.0, 0.4);
        let inner = frame.shrink(8.0);
        let k = (inner.width() / size.0 as f32).min(inner.height() / size.1 as f32);
        let img = Rect::from_center_size(inner.center(), vec2(size.0 as f32 * k, size.1 as f32 * k));
        rounded_image(&p, &tex, img, 9.0);

        // разметка: протянуть мышью по миниатюре — красная рамка вокруг важного
        const MARK: Color32 = Color32::from_rgb(0xFF, 0x4D, 0x4D);
        let draw = ui.interact(img, ui.id().with("shot-mark"), Sense::drag()).on_hover_cursor(egui::CursorIcon::Crosshair);
        if draw.drag_started() {
            self.shot_drag = draw.interact_pointer_pos();
        }
        let to_view = |m: &[u32; 4]| {
            let (sx, sy) = (img.width() / size.0 as f32, img.height() / size.1 as f32);
            Rect::from_min_max(pos2(img.left() + m[0] as f32 * sx, img.top() + m[1] as f32 * sy), pos2(img.left() + m[2] as f32 * sx, img.top() + m[3] as f32 * sy))
        };
        let clip = p.with_clip_rect(img);
        for m in &self.shot_marks {
            clip.rect_stroke(to_view(m), Rounding::same(2.0), Stroke::new(2.0_f32, MARK));
        }
        if let (Some(a), Some(b)) = (self.shot_drag, ui.ctx().pointer_latest_pos()) {
            if let Some(m) = view_to_image(a, b, img, size) {
                let r = to_view(&m);
                clip.rect_filled(r, Rounding::same(2.0), MARK.gamma_multiply(0.12));
                clip.rect_stroke(r, Rounding::same(2.0), Stroke::new(2.0_f32, MARK));
            }
            if draw.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
                if let Some(m) = view_to_image(a, b, img, size) {
                    self.shot_marks.push(m);
                }
                self.shot_drag = None;
            }
        }
        if self.shot_marks.is_empty() && draw.hovered() && self.shot_drag.is_none() {
            let tip = Rect::from_center_size(pos2(img.center().x, img.bottom() - 14.0), vec2(176.0, 20.0));
            p.rect_filled(tip, Rounding::same(10.0), Color32::from_rgba_unmultiplied(10, 10, 18, 190));
            p.text(tip.center(), Align2::CENTER_CENTER, "Протяните, чтобы обвести", FontId::proportional(11.5), FG);
        }

        let x = frame.right() + 18.0;
        p.text(pos2(x, row.top() + 4.0), Align2::LEFT_TOP, "Снимок сохранён", semi(17.0), FG);
        p.with_clip_rect(Rect::from_min_max(pos2(x, row.top()), row.max))
            .text(pos2(x, row.top() + 30.0), Align2::LEFT_TOP, format!("{} × {} · уже в буфере обмена", size.0, size.1), FontId::proportional(12.5), DIM);
        p.with_clip_rect(Rect::from_min_max(pos2(x, row.top()), row.max))
            .text(pos2(x, row.top() + 48.0), Align2::LEFT_TOP, &name, FontId::proportional(12.0), mix(DIM, BG, 0.25));
        let mut act = None;
        let mut side = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(x, row.top() + 74.0), row.max)));
        let marked = !self.shot_marks.is_empty();
        side.horizontal_wrapped(|ui| {
            if marked {
                // разметка есть — сначала её кнопки, красным
                let n = self.shot_marks.len();
                if jelly(ui, &format!("Применить ({n})")).on_hover_text("Нарисовать рамки на снимке, сохранить файл и снова положить в буфер").clicked() {
                    act = Some("apply");
                }
                if jelly(ui, "Отменить").on_hover_text("Убрать последнюю рамку").clicked() {
                    act = Some("undo");
                }
            }
            for (id, label, tip) in [
                ("text", "Скопировать текст", "Распознать текст на снимке и положить его в буфер обмена"),
                ("open", "Открыть", "Открыть снимок в программе просмотра"),
                ("folder", "Показать в папке", "Открыть папку со снимком"),
                ("shelf", "На полку", "Положить снимок на полку острова"),
                ("delete", "Удалить", "Удалить файл снимка"),
            ] {
                if jelly(ui, label).on_hover_text(tip).clicked() {
                    act = Some(id);
                }
            }
        });
        // пока курсор над карточкой, она не сворачивается
        if ui.rect_contains_pointer(ui.max_rect()) {
            if let Some(c) = &mut self.shot_card {
                c.until = Instant::now() + Duration::from_secs(8);
            }
        }
        match act {
            Some("undo") => {
                self.shot_marks.pop();
            }
            Some("apply") => {
                // толщина рамки — от размера снимка, чтобы на 4K она не терялась
                let t = (size.0.max(size.1) / 400).max(3);
                let mut px = (*rgba).clone();
                for m in &self.shot_marks {
                    draw_frame_rgba(&mut px, size.0, size.1, *m, t, [MARK.r(), MARK.g(), MARK.b()]);
                }
                // исходник не трогаем: размеченное — отдельным файлом рядом. Проверочный запуск ничего не пишет на диск
                let out = marked_path(&path, |p| p.exists());
                let saved = !test_mode() && save_png_atomic(&out, &px, size.0, size.1).is_ok();
                self.clip_skip = true;
                let data = arboard::ImageData { width: size.0, height: size.1, bytes: std::borrow::Cow::Borrowed(&px) };
                let copied = self.clipboard.as_mut().is_some_and(|cb| cb.set_image(data).is_ok());
                // карточка дальше работает с новым файлом; не сохранился — остаётся прежний путь
                let shown = if saved { out.to_string_lossy().to_string() } else { path };
                self.show_shot(ui.ctx(), size.0, size.1, px, shown);
                self.notify(match (saved, copied) {
                    (true, true) => "Разметка сохранена и в буфере",
                    (true, false) => "Разметка сохранена, в буфер не вышло",
                    (false, true) => "Разметка в буфере, файл сохранить не вышло",
                    (false, false) => "Не удалось сохранить разметку",
                });
            }
            Some("text") => {
                self.notify("Распознаю текст…");
                let (tx, ctx) = (self.tx.clone(), ui.ctx().clone());
                std::thread::spawn(move || {
                    let res = sys::ocr(size.0, size.1, &rgba).and_then(|text| {
                        if text.trim().is_empty() {
                            return Err("На снимке текста не найдено".to_string());
                        }
                        let n = text.chars().count();
                        arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text)).map_err(|e| e.to_string())?;
                        Ok(n)
                    });
                    let _ = tx.send(Msg::Note(match res {
                        Ok(n) => format!("Текст скопирован: {n} симв."),
                        Err(e) => e,
                    }));
                    ctx.request_repaint();
                });
            }
            Some("open") => {
                sys::open(&path);
                self.mode = Mode::Pill;
            }
            Some("folder") => {
                let _ = std::process::Command::new("explorer.exe").arg(format!("/select,{path}")).spawn();
                self.mode = Mode::Pill;
            }
            Some("shelf") => {
                if !self.cfg.shelf.contains(&path) {
                    self.cfg.shelf.insert(0, path);
                    self.cfg.save();
                }
                self.notify("Снимок на полке");
            }
            Some("delete") => {
                let _ = std::fs::remove_file(&path);
                self.shot_card = None;
                self.mode = Mode::Pill;
                self.notify("Снимок удалён");
            }
            _ => {}
        }
    }

    /// Выполняет подсказанное действие: открыть ссылку или папку, либо положить результат в буфер.
    fn do_hint(&mut self, act: HintAct) {
        match act {
            HintAct::Open(target) => {
                sys::open(&target);
                self.mode = Mode::Pill;
            }
            HintAct::Copy(text) => {
                self.set_clipboard(&text);
                self.notify("Скопировано в буфер");
            }
        }
        self.hint_until = Instant::now();
    }

    /// Записи → Заметки: слева список (название — первая строка), справа текст выбранной.
    fn ui_notes(&mut self, ui: &mut egui::Ui) {
        if self.notes.is_empty() {
            self.notes.push(String::new());
        }
        self.note_sel = self.note_sel.min(self.notes.len() - 1);
        let q = self.note_query.trim().to_lowercase();
        let (mut pick, mut add, mut del) = (None, false, false);
        let full = ui.available_rect_before_wrap();
        let left = Rect::from_min_size(full.min, vec2(204.0, full.height()));
        let right = Rect::from_min_max(pos2(left.right() + 12.0, full.top()), full.max);
        ui.allocate_rect(full, Sense::hover());

        // ----- список
        let mut lu = ui.new_child(egui::UiBuilder::new().max_rect(left));
        lu.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.note_query)
                    .margin(egui::Margin::symmetric(9.0, 7.0))
                    .hint_text(RichText::new("Поиск").color(DIM))
                    .desired_width(left.width() - 56.0),
            );
            add = jelly(ui, "+").on_hover_text("Новая заметка").clicked();
        });
        egui::ScrollArea::vertical().id_salt("notes-list").auto_shrink([false, false]).show(&mut lu, |ui| {
            let mut shown = 0;
            for (i, n) in self.notes.iter().enumerate() {
                if !q.is_empty() && !n.to_lowercase().contains(&q) {
                    continue;
                }
                shown += 1;
                let mut lines = n.lines().map(str::trim).filter(|l| !l.is_empty());
                let title = lines.next().unwrap_or("Новая заметка").to_string();
                let more = store::preview_line(lines.next().unwrap_or(""));
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.1);
                let on = i == self.note_sel;
                glass_chip(ui.painter(), r, 11.0, if on { 1.0 } else { 0.3 + 0.4 * hov });
                let p = ui.painter().with_clip_rect(r.shrink2(vec2(10.0, 0.0)));
                let (done, total) = store::check_progress(n);
                let p = if total > 0 {
                    let badge = p.text(pos2(r.right() - 10.0, r.top() + 14.0), Align2::RIGHT_CENTER, format!("{done}/{total}"), FontId::proportional(11.0), if done == total { accent() } else { DIM });
                    p.with_clip_rect(Rect::from_min_max(r.min + vec2(10.0, 0.0), pos2(badge.left() - 6.0, r.bottom())))
                } else {
                    p
                };
                p.text(pos2(r.left() + 12.0, r.top() + 14.0), Align2::LEFT_CENTER, title, semi(13.0), if on { accent() } else { FG });
                p.text(pos2(r.left() + 12.0, r.top() + 31.0), Align2::LEFT_CENTER, if more.is_empty() { "без продолжения".to_string() } else { more }, FontId::proportional(11.5), DIM);
                if resp.clicked() {
                    pick = Some(i);
                }
                ui.add_space(3.0);
            }
            if shown == 0 {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("Ничего не найдено").color(DIM)));
            }
        });

        // ----- текст
        let mut ru = ui.new_child(egui::UiBuilder::new().max_rect(right));
        let sel = self.note_sel;
        let mut toggle = None;
        egui::ScrollArea::vertical().id_salt("notes-text").max_height(right.height() - 30.0).auto_shrink([false, false]).show(&mut ru, |ui| {
            // ----- чек-лист: пункты «- [ ] …» кружками, щелчок отмечает
            let items = store::check_items(&self.notes[sel]);
            if !items.is_empty() {
                let (done, total) = (items.iter().filter(|c| c.done).count(), items.len());
                let (br, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
                let bar = Rect::from_min_size(br.min, vec2(br.width(), 3.0));
                ui.painter().rect_filled(bar, 2.0, DIM.gamma_multiply(0.25));
                let k = ui.ctx().animate_value_with_time(Id::new(("chk-bar", sel)), done as f32 / total as f32, 0.25);
                ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * k, 3.0)), 2.0, accent());
                ui.add_space(4.0);
                // редактору под пунктами оставляем не меньше ~4 строк, лишние пункты прокручиваются
                let cap = (right.height() - 30.0 - 30.0 - 4.0 * 18.0 - 30.0).max(52.0);
                egui::ScrollArea::vertical().id_salt(("notes-checks", sel)).max_height(cap).auto_shrink([false, true]).show(ui, |ui| {
                for c in &items {
                    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
                    let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.1);
                    let t = ui.ctx().animate_bool_with_time(Id::new(("chk", sel, c.line)), c.done, 0.18);
                    if hov > 0.0 {
                        glass_chip(ui.painter(), r, 8.0, 0.35 * hov);
                    }
                    let ctr = pos2(r.left() + 16.0, r.center().y);
                    ui.painter().circle_stroke(ctr, 7.5, Stroke::new(1.5_f32, if c.done { accent() } else { DIM }));
                    if t > 0.0 {
                        ui.painter().circle_filled(ctr, 7.5 * t, accent());
                        let pts = vec![ctr + vec2(-3.5, 0.2), ctr + vec2(-1.0, 2.8), ctr + vec2(3.8, -2.6)];
                        ui.painter().add(egui::Shape::line(pts, Stroke::new(1.6_f32, Color32::WHITE.gamma_multiply(t))));
                    }
                    let label = if c.label.is_empty() { "пустой пункт" } else { c.label.as_str() };
                    let g = ui.painter().with_clip_rect(r).text(pos2(r.left() + 32.0, r.center().y), Align2::LEFT_CENTER, label, FontId::proportional(13.0), if c.done { DIM } else { FG });
                    if t > 0.0 {
                        let y = g.center().y;
                        ui.painter().with_clip_rect(r).line_segment([pos2(g.left(), y), pos2(g.left() + g.width() * t, y)], Stroke::new(1.0_f32, DIM));
                    }
                    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        toggle = Some(c.line);
                    }
                }
                });
                ui.label(RichText::new(format!("Готово {done} из {total}")).color(DIM).size(11.0));
                ui.add_space(6.0);
            }
            let r = ui.add(
                egui::TextEdit::multiline(&mut self.notes[sel])
                    .hint_text(RichText::new("Первая строка станет названием. Заметки сохраняются сами").color(DIM))
                    .desired_width(f32::INFINITY)
                    .desired_rows(11)
                    .margin(egui::Margin::symmetric(10.0, 8.0)),
            );
            if r.changed() {
                self.notes_dirty = Some(Instant::now());
                toggle = None; // текст сдвинулся — номер строки мог устареть
            }
        });
        ru.horizontal(|ui| {
            let n = self.notes[sel].chars().count();
            ui.label(RichText::new(format!("{} из {} · {n} симв.", sel + 1, self.notes.len())).color(DIM).size(11.5));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                del = ui.add_enabled(self.notes.len() > 1 || n > 0, egui::Button::new("Удалить")).clicked();
                if jelly(ui, "+ пункт").on_hover_text("Добавить пункт чек-листа «- [ ] »").clicked() {
                    self.notes[sel] = store::append_check(&self.notes[sel]);
                    self.notes_dirty = Some(Instant::now());
                }
                if jelly(ui, "Копировать").clicked() {
                    let text = self.notes[sel].clone();
                    self.set_clipboard(&text);
                    self.notify("Заметка скопирована");
                }
            });
        });

        if let Some(line) = toggle {
            self.notes[sel] = store::toggle_check(&self.notes[sel], line);
            self.notes_dirty = Some(Instant::now());
        }
        if let Some(i) = pick {
            self.note_sel = i;
        }
        if add {
            // пустую заметку второй раз не создаём — просто открываем её
            match self.notes.iter().position(|n| n.trim().is_empty()) {
                Some(i) => self.note_sel = i,
                None => {
                    self.notes.insert(0, String::new());
                    self.note_sel = 0;
                }
            }
            self.note_query.clear();
        }
        if del {
            self.notes.remove(sel);
            if self.notes.is_empty() {
                self.notes.push(String::new());
            }
            self.note_sel = sel.min(self.notes.len() - 1);
            self.notes_dirty = Some(Instant::now());
            self.notify("Заметка удалена");
        }
    }

    /// Настройки в две колонки: слева вид и данные, справа — поведение переключателями.
    fn ui_settings(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let head = |ui: &mut egui::Ui, text: &str| {
            ui.label(RichText::new(text).font(semi(11.5)).color(DIM));
        };
        let mut check_update = false;
        ui.columns(2, |c| {
            // ── слева: вид
            let ui = &mut c[0];
            ui.set_max_width(ui.available_width() - 10.0);
            head(ui, "ТЕМА");
            ui.horizontal(|ui| {
                let cur = THEME.load(Ordering::Relaxed);
                let mut hovered: Option<usize> = std::env::var("OSTROVOK_THEMEHOVER").ok().filter(|_| test_mode()).and_then(|v| v.parse().ok());
                for (i, t) in THEMES.iter().enumerate() {
                    let (r, resp) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
                    if resp.hovered() {
                        hovered = Some(i);
                    }
                    let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.1);
                    let sel = ui.ctx().animate_bool_with_time(resp.id.with("sel"), i == cur, 0.14);
                    let col = Color32::from_rgb(t.accent[0], t.accent[1], t.accent[2]);
                    // кружок показывает и акцент, и оттенок стекла
                    let [gr, gg, gb] = glass_shade(t.glass);
                    ui.painter().circle_filled(r.center(), 11.0 + hov, Color32::from_rgb(gr, gg, gb));
                    ui.painter().circle_filled(r.center(), 7.0 + hov, col);
                    if sel > 0.0 {
                        ui.painter().circle_stroke(r.center(), 13.5, Stroke::new(1.6_f32, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), (sel * 255.0) as u8)));
                    }
                    if resp.clicked() {
                        THEME.store(i, Ordering::Relaxed);
                        self.cfg.theme = i as u8;
                        self.cfg.save();
                    }
                }
                // мини-пилюля: как остров будет выглядеть в теме под курсором (или в выбранной)
                ui.add_space(6.0);
                let idx = theme_preview_idx(hovered, cur);
                let t = &THEMES[idx];
                // ширина — под самое длинное название, чтобы пилюля не прыгала и не вылезала из колонки
                let name_w = THEMES.iter().map(|t| ui.painter().layout_no_wrap(t.name.to_string(), semi(11.5), FG).size().x).fold(0.0, f32::max);
                let (r, _) = ui.allocate_exact_size(vec2(theme_preview_w(name_w).min(ui.available_width()), 28.0), Sense::hover());
                let fade = |key: &str, v: u8| ui.ctx().animate_value_with_time(ui.id().with(("tp", key)), v as f32, 0.18) as u8;
                let [gr, gg, gb] = t.glass;
                let glass = Color32::from_rgb(fade("gr", gr), fade("gg", gg), fade("gb", gb));
                let a = Color32::from_rgb(fade("ar", t.accent[0]), fade("ag", t.accent[1]), fade("ab", t.accent[2]));
                let p = ui.painter();
                p.rect_filled(r, Rounding::same(14.0), glass);
                p.rect_stroke(r, Rounding::same(14.0), Stroke::new(1.2_f32, Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), 150)));
                for dx in [11.0, 18.0] {
                    p.rect_filled(Rect::from_center_size(pos2(r.left() + dx, r.center().y), vec2(3.5, 7.0)), Rounding::same(1.8), Color32::WHITE);
                }
                p.text(pos2(r.left() + 28.0, r.center().y), Align2::LEFT_CENTER, t.name, semi(11.5), a);
            });
            ui.add_space(4.0);
            head(ui, "ПЕРСОНАЖ");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(5.0, 5.0);
                for (i, name) in ["Глазки", "Робот", "Кот", "Призрак", "Слайм", "Лисёнок"].into_iter().enumerate() {
                    let on = self.cfg.character == i as u8;
                    let width = ui.painter().layout_no_wrap(name.to_string(), semi(12.5), FG).size().x + 20.0;
                    let (r, resp) = ui.allocate_exact_size(vec2(width, 28.0), Sense::click());
                    let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.1);
                    glass_chip(ui.painter(), r, 10.0, if on { 1.1 } else { 0.4 + 0.4 * hov });
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, name, semi(12.5), if on { accent() } else { FG });
                    if resp.clicked() {
                        self.cfg.character = i as u8;
                        self.cfg.save();
                    }
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                head(ui, "ПРОЗРАЧНОСТЬ");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("{}%", self.clearness())).size(11.5).color(DIM));
                });
            });
            {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::hover());
                let mut v = self.clearness() as f32 / 100.0;
                if slim_slider(ui, ui.id().with("clear"), r.shrink2(vec2(4.0, 2.0)), &mut v, accent(), false) {
                    self.cfg.clear = ((v * 100.0).round() as u8).clamp(1, 100);
                }
                // сохраняем, когда ползунок отпустили
                if ui.input(|i| i.pointer.any_released()) && self.cfg.clear != self.clear_saved {
                    self.clear_saved = self.cfg.clear;
                    self.cfg.save();
                }
            }
            ui.add_space(4.0);
            head(ui, "ГОРОД ДЛЯ ПОГОДЫ");
            ui.horizontal(|ui| {
                let r = ui.add(
                    egui::TextEdit::singleline(&mut self.city_edit)
                        .margin(egui::Margin::symmetric(9.0, 7.0))
                        .hint_text(RichText::new("Например, Казань").color(DIM))
                        .desired_width(ui.available_width() - 134.0),
                );
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if jelly(ui, "Сохранить").on_hover_text("Название города отправляется на open-meteo.com. Пустое поле отключает погоду.").clicked() || enter {
                    self.save_city(ctx);
                }
            });
            ui.add_space(2.0);
            head(ui, "КЛЮЧ CLAUDE API ДЛЯ ИИ");
            let key_state = if self.api_key().is_empty() { "не задан" } else { "задан" };
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.key_edit)
                        .margin(egui::Margin::symmetric(9.0, 7.0))
                        .password(true)
                        .hint_text(RichText::new(key_state).color(DIM))
                        .desired_width(ui.available_width() - 134.0),
                );
                if jelly(ui, "Сохранить").clicked() {
                    self.save_key();
                }
            });
            ui.add_space(8.0);
            // ── установка: карточка, пока остров запущен не из своей папки
            if !self.is_installed {
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 62.0), Sense::hover());
                let p = ui.painter();
                glass_chip(p, r, 16.0, 0.7);
                icon_disc(p, pos2(r.left() + 30.0, r.center().y), 17.0, [150, 200, 255], [70, 120, 230], "\u{E896}", 15.0);
                p.text(pos2(r.left() + 58.0, r.top() + 14.0), Align2::LEFT_TOP, "Установить Островок", semi(13.5), FG);
                p.text(pos2(r.left() + 58.0, r.top() + 34.0), Align2::LEFT_TOP, "ярлык в меню «Пуск»", FontId::proportional(11.5), DIM);
                let b = Rect::from_min_max(pos2(r.right() - 104.0, r.top() + 15.0), pos2(r.right() - 12.0, r.bottom() - 15.0));
                let resp = ui
                    .interact(b, ui.id().with("install"), Sense::click())
                    .on_hover_text("Остров переедет в %LOCALAPPDATA%\\Programs\\Ostrovok, появится в меню «Пуск», автозапуск переедет вместе с ним");
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
                let p = ui.painter();
                p.rect_filled(b, Rounding::same(11.0), mix(accent(), Color32::WHITE, 0.12 * hov));
                p.text(b.center(), Align2::CENTER_CENTER, "Установить", semi(12.5), Color32::from_rgb(20, 18, 32));
                if resp.clicked() {
                    match if test_mode() { Err("В проверочном запуске установка отключена".to_string()) } else { install::install() } {
                        Ok(new) => {
                            store::save_notes(&self.notes);
                            match std::process::Command::new(&new).arg("--updated").spawn() {
                                Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                                Err(e) => self.notify(&format!("Скопировал, но не запустил: {e}")),
                            }
                        }
                        Err(e) => self.notify(&e),
                    }
                }
            }

            // ── справа: поведение
            let ui = &mut c[1];
            head(ui, "ПОВЕДЕНИЕ");
            if switch_row(ui, &mut self.cfg.lyrics, "Текст песен", "название трека уходит на lrclib.net") {
                self.cfg.save();
                self.lyrics_key.clear();
                self.lyrics.clear();
            }
            let mirror_hint = if !self.cfg.mirror {
                "Windows спросит разрешение"
            } else {
                match self.live.access.load(Ordering::Relaxed) {
                    1 => "доступ к уведомлениям есть",
                    2 => "Windows не дала доступ к уведомлениям",
                    3 => "на этой системе недоступно",
                    _ => "жду ответа Windows…",
                }
            };
            if switch_row(ui, &mut self.cfg.mirror, "Уведомления мессенджеров", mirror_hint) {
                self.cfg.save();
                self.live.mirror.store(self.cfg.mirror, Ordering::Relaxed);
            }
            if switch_row(ui, &mut self.cfg.capture_visible, "Виден на записи экрана", "стекло перестанет размывать фон; в звонке Discord виден и так") {
                self.cfg.save();
            }
            if switch_row(ui, &mut self.cfg.over_games, "Поверх полноэкранных игр", "работает, если игра в режиме окна без рамки") {
                self.cfg.save();
            }
            let mut wx = !self.cfg.no_wx;
            if switch_row(ui, &mut wx, "Живая погода в пилюле", "") {
                self.cfg.no_wx = !wx;
                self.cfg.save();
            }
            let mut hud = !self.cfg.no_hud;
            if switch_row(ui, &mut hud, "Громкость и раскладка в пилюле", "") {
                self.cfg.no_hud = !hud;
                self.cfg.save();
            }
            if switch_row(ui, &mut self.autostart, "Запуск вместе с Windows", "") {
                store::set_autostart(self.autostart);
            }
            let state = self.upd.lock().unwrap().clone();
            let note = match &state {
                update::State::Checking => "проверяю…".to_string(),
                update::State::Fresh => "установлена последняя версия".to_string(),
                update::State::Ready(v, _) => format!("версия {v} скачана, поставится сама"),
                update::State::Failed(e) => e.clone(),
                update::State::Idle => "с GitHub, раз в минуту".to_string(),
            };
            let mut auto = !self.cfg.no_update;
            if switch_row(ui, &mut auto, "Обновляться автоматически", &note) {
                self.cfg.no_update = !auto;
                self.cfg.save();
            }
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if jelly(ui, "Проверить обновления").clicked() && !matches!(state, update::State::Checking) {
                    check_update = true;
                }
                if jelly(ui, "Подсказки").on_hover_text("Ещё раз показать, что умеет остров").clicked() {
                    self.intro = Some((0, Instant::now() + Duration::from_millis(600)));
                    self.mode = Mode::Pill;
                }
            });
        });
        if check_update {
            *self.upd.lock().unwrap() = update::State::Idle;
            let c = ui.ctx().clone();
            update::spawn(self.upd.clone(), move || c.request_repaint());
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if jelly(ui, "Закрыть Островок").clicked() {
                store::save_notes(&self.notes);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ui.label(RichText::new(format!("Островок {} · поиск по {} · щелчок по персонажу меняет настроение", env!("CARGO_PKG_VERSION"), self.hotkey_name)).color(DIM).size(11.5));
        });
    }

    /// Прозрачность стекла в процентах (в настройках 0 значит «не трогали»).
    fn clearness(&self) -> u8 {
        if test_mode() {
            if let Some(v) = std::env::var("OSTROVOK_CLEAR").ok().and_then(|v| v.parse::<u8>().ok()) {
                return v;
            }
        }
        if self.cfg.clear == 0 { 60 } else { self.cfg.clear }
    }

    fn api_key(&self) -> String {
        match std::env::var("ANTHROPIC_API_KEY") {
            Ok(k) if !k.trim().is_empty() => k.trim().to_string(),
            _ => self.cfg.api_key.trim().to_string(),
        }
    }

    fn save_key(&mut self) {
        self.cfg.api_key = self.key_edit.trim().to_string();
        self.cfg.save();
        self.key_edit.clear();
        self.notify(if self.cfg.api_key.is_empty() { "Ключ удалён" } else { "Ключ сохранён" });
    }

    /// Открывает выбранный чат во вкладке «ИИ»; вопрос подставляется там, где сайт это умеет.
    fn open_ai_site(&mut self, question: &str) {
        let q = question.trim().to_string();
        if self.cfg.ai_site == "gemini" && !q.is_empty() {
            self.set_clipboard(&q);
            self.notify("Вопрос в буфере — вставьте его: Ctrl + V");
        }
        self.web_nav = Some(site_url(&self.cfg.ai_site, &q));
        self.mode = Mode::Panel;
        self.tab = Tab::Ai;
        self.sticky = true;
    }

    /// Создаёт, двигает, показывает и прячет встроенное веб-окно чата.
    fn sync_web(&mut self, frame: &eframe::Frame, ctx: &egui::Context, slot: Option<Rect>) {
        let Some(slot) = slot else {
            if self.web_visible {
                if let Some(w) = &self.web {
                    let _ = w.set_visible(false);
                }
                self.web_visible = false;
            }
            return;
        };
        if self.web_error.is_some() {
            return;
        }
        let ppp = ctx.pixels_per_point();
        let bounds = wry::Rect {
            position: wry::dpi::PhysicalPosition::new((slot.min.x * ppp).round() as i32, (slot.min.y * ppp).round() as i32).into(),
            size: wry::dpi::PhysicalSize::new((slot.width() * ppp).round() as u32, (slot.height() * ppp).round() as u32).into(),
        };
        if self.web.is_none() {
            let url = self.web_nav.take().unwrap_or_else(|| site_url(&self.cfg.ai_site, ""));
            // отдельный профиль: вход в аккаунт хранится в папке настроек Островка
            let mut wc = Box::new(wry::WebContext::new(Some(store::dir().join("webview"))));
            let built = wry::WebViewBuilder::with_web_context(&mut wc)
                .with_url(url)
                .with_bounds(bounds)
                .with_background_color((20, 20, 24, 255))
                .with_focused(false)
                .build_as_child(frame);
            match built {
                Ok(w) => {
                    self.web = Some(w);
                    self.web_ctx = Some(wc);
                    self.web_rect = Some(slot);
                    self.web_visible = true;
                }
                Err(e) => self.web_error = Some(e.to_string()),
            }
            return;
        }
        if let Some(w) = &self.web {
            if let Some(url) = self.web_nav.take() {
                let _ = w.load_url(&url);
            }
            if self.web_rect != Some(slot) {
                let _ = w.set_bounds(bounds);
                self.web_rect = Some(slot);
            }
            if !self.web_visible {
                let _ = w.set_visible(true);
                self.web_visible = true;
            }
        }
    }

    fn ask(&mut self, question: String) {
        let q = question.trim().to_string();
        if q.is_empty() || self.ai_busy {
            return;
        }
        let key = self.api_key();
        if key.is_empty() {
            self.ai_input = q;
            return;
        }
        self.ai_chat.push((true, q));
        if self.ai_chat.len() > 40 {
            let extra = self.ai_chat.len() - 40;
            self.ai_chat.drain(..extra + extra % 2);
        }
        self.ai_busy = true;
        let (tx, history) = (self.tx.clone(), self.ai_chat.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Ai(ai::ask(&key, &history)));
        });
    }

    fn ui_ai(&mut self, ui: &mut egui::Ui) {
        if self.api_key().is_empty() {
            let sites = [("claude", "Claude"), ("gemini", "Gemini"), ("chatgpt", "ChatGPT")];
            let cur = if sites.iter().any(|s| s.0 == self.cfg.ai_site) { self.cfg.ai_site.clone() } else { "claude".to_string() };
            let (mut pick, mut reload, mut external) = (None, false, false);
            ui.horizontal(|ui| {
                for (id, name) in sites {
                    let label = if id == cur { format!("●  {name}") } else { name.to_string() };
                    if jelly(ui, label).clicked() {
                        pick = Some(id);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    external = jelly(ui, "В браузере").clicked();
                    reload = jelly(ui, "Обновить").clicked();
                });
            });
            if let Some(id) = pick {
                self.cfg.ai_site = id.to_string();
                self.cfg.save();
                self.web_nav = Some(site_url(id, ""));
            }
            if reload {
                self.web_nav = Some(site_url(&cur, ""));
            }
            if external {
                sys::open(&site_url(&cur, ""));
            }
            let slot = ui.available_rect_before_wrap();
            match &self.web_error {
                Some(err) => {
                    ui.add_space(30.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("Не удалось встроить чат в панель").font(semi(16.5)));
                        ui.label(RichText::new("Нужен системный компонент Microsoft Edge WebView2. Пока можно открыть чат кнопкой «В браузере».").color(DIM));
                        ui.label(RichText::new(err.as_str()).color(DIM).size(11.5));
                    });
                }
                None => {
                    self.web_slot = Some(slot);
                    if self.web.is_none() {
                        ui.painter().text(slot.center(), Align2::CENTER_CENTER, "Загружаю чат…", FontId::proportional(13.5), DIM);
                    }
                    ui.allocate_rect(slot, Sense::hover());
                }
            }
            return;
        }
        let list_h = (ui.available_height() - 36.0).max(60.0);
        egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            if self.ai_chat.is_empty() && !self.ai_busy {
                ui.add_space(40.0);
                ui.vertical_centered(|ui| ui.label(RichText::new("Спросите что угодно. Переписка хранится только до выхода.").color(DIM)));
            }
            for (me, text) in &self.ai_chat {
                if *me {
                    ui.label(RichText::new(text).color(accent()));
                } else {
                    ui.label(RichText::new(text).color(FG));
                    ui.add_space(4.0);
                }
            }
            if self.ai_busy {
                ui.label(RichText::new("Думаю…").color(DIM));
            }
        });
        let mut send = false;
        let mut copy = false;
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 250.0).max(120.0);
            let r = ui.add(egui::TextEdit::singleline(&mut self.ai_input).margin(egui::Margin::symmetric(9.0, 7.0)).hint_text(RichText::new("Ваш вопрос").color(DIM)).desired_width(w));
            send = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            send |= ui.add_enabled(!self.ai_busy, egui::Button::new("Спросить")).clicked();
            copy = jelly(ui, "Копировать").clicked();
            if jelly(ui, "Очистить").clicked() && !self.ai_busy {
                self.ai_chat.clear();
            }
        });
        if send {
            let q = std::mem::take(&mut self.ai_input);
            self.ask(q);
        }
        if copy {
            if let Some(text) = self.ai_chat.iter().rev().find(|m| !m.0).map(|m| m.1.clone()) {
                self.set_clipboard(&text);
                self.notify("Ответ скопирован");
            }
        }
    }

    /// Обзор → Календарь: слева месяц (дни с делами отмечены точкой), справа — выбранный день и его напоминания.
    fn ui_calendar(&mut self, ui: &mut egui::Ui) {
        use chrono::{Datelike, Local, NaiveDate, NaiveTime};
        const MONTHS: [&str; 12] =
            ["Январь", "Февраль", "Март", "Апрель", "Май", "Июнь", "Июль", "Август", "Сентябрь", "Октябрь", "Ноябрь", "Декабрь"];
        const OF_MONTH: [&str; 12] =
            ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];
        const DAYS: [&str; 7] = ["понедельник", "вторник", "среда", "четверг", "пятница", "суббота", "воскресенье"];
        let today = Local::now().date_naive();
        let sel = self.cal_sel.unwrap_or(today);
        let index = today.year() * 12 + today.month0() as i32 + self.cal_shift;
        let (year, month0) = (index.div_euclid(12), index.rem_euclid(12) as u32);
        let Some(first) = NaiveDate::from_ymd_opt(year, month0 + 1, 1) else { return };
        let next = if month0 == 11 { NaiveDate::from_ymd_opt(year + 1, 1, 1) } else { NaiveDate::from_ymd_opt(year, month0 + 2, 1) };
        let days = next.map_or(30, |n| (n - first).num_days()) as u32;
        let offset = first.weekday().num_days_from_monday();
        // на какой день приходится каждое напоминание
        let dated: Vec<(usize, NaiveDate, String, String)> = self
            .cfg
            .reminders
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                let d = chrono::DateTime::from_timestamp(r.at, 0)?.with_timezone(&Local);
                Some((i, d.date_naive(), d.format("%H:%M").to_string(), r.text.clone()))
            })
            .collect();

        ui.horizontal(|ui| {
            if jelly(ui, "‹").clicked() {
                self.cal_shift -= 1;
            }
            if jelly(ui, "›").clicked() {
                self.cal_shift += 1;
            }
            ui.label(RichText::new(format!("{} {year}", MONTHS[month0 as usize])).font(semi(15.0)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if (self.cal_shift != 0 || self.cal_sel.is_some_and(|d| d != today)) && jelly(ui, "Сегодня").clicked() {
                    self.cal_shift = 0;
                    self.cal_sel = None;
                }
            });
        });

        let (mut click, mut remove, mut add) = (None, None, false);
        ui.columns(2, |c| {
            // ----- месяц
            let ui = &mut c[0];
            let cell = vec2(ui.available_width() / 7.0, 31.0);
            let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0 + cell.y * 6.0), Sense::hover());
            let origin = area.min;
            let p = ui.painter().clone();
            // выходные — мягким коралловым, чтобы неделя читалась с первого взгляда
            let weekend = Color32::from_rgb(0xFF, 0x9A, 0x8A);
            for (i, name) in ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"].into_iter().enumerate() {
                p.text(origin + vec2(cell.x * (i as f32 + 0.5), 10.0), Align2::CENTER_CENTER, name, FontId::proportional(11.5), if i >= 5 { mix(weekend, DIM, 0.25) } else { DIM });
            }
            for day in 1..=days {
                let Some(date) = NaiveDate::from_ymd_opt(year, month0 + 1, day) else { continue };
                let slot = offset + day - 1;
                let (col, row) = (slot % 7, slot / 7);
                let at = origin + vec2(cell.x * (col as f32 + 0.5), 24.0 + cell.y * (row as f32 + 0.5));
                let resp = ui.interact(Rect::from_center_size(at, vec2(cell.x.min(40.0), 29.0)), ui.id().with(("cal-day", year, month0, day)), Sense::click());
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.1);
                let (is_today, is_sel) = (date == today, date == sel);
                let fest = ru_holiday(month0 + 1, day).is_some();
                if is_today {
                    p.circle_filled(at, 13.0, accent());
                } else if is_sel {
                    p.circle_filled(at, 13.0, rgba(255, 255, 255, 26));
                    p.circle_stroke(at, 13.0, Stroke::new(1.5_f32, accent()));
                } else if hov > 0.0 {
                    p.circle_filled(at, 13.0, rgba(255, 255, 255, (26.0 * hov) as u8));
                }
                if is_today && is_sel && self.cal_sel.is_some() {
                    p.circle_stroke(at, 15.5, Stroke::new(1.2_f32, accent()));
                }
                let color = if is_today {
                    BG
                } else if date < today {
                    mix(if col >= 5 || fest { mix(weekend, DIM, 0.4) } else { DIM }, BG, 0.4)
                } else if col >= 5 || fest {
                    weekend
                } else {
                    FG
                };
                p.text(at, Align2::CENTER_CENTER, day.to_string(), semi(13.5), color);
                // праздник — маленькая коралловая звёздочка справа сверху
                if fest {
                    let star = at + vec2(10.5, -9.5);
                    let tone = if date < today { mix(weekend, BG, 0.4) } else { weekend };
                    p.circle_filled(star, 2.6, tone);
                    p.circle_filled(star, 4.5, Color32::from_rgba_unmultiplied(weekend.r(), weekend.g(), weekend.b(), 40));
                }
                // точки — сколько на этот день дел (не больше трёх)
                let n = dated.iter().filter(|d| d.1 == date).count().min(3);
                for k in 0..n {
                    let dx = (k as f32 - (n as f32 - 1.0) / 2.0) * 5.0;
                    // у сегодняшнего дня точки под кружком, чтобы не налезали на число
                    p.circle_filled(at + vec2(dx, if is_today { 16.5 } else { 11.0 }), 1.8, accent());
                }
                if resp.clicked() {
                    click = Some(date);
                }
            }

            // ----- выбранный день
            let ui = &mut c[1];
            ui.add_space(4.0);
            let gap = (sel - today).num_days();
            let when = match gap {
                0 => "сегодня".to_string(),
                1 => "завтра".to_string(),
                -1 => "вчера".to_string(),
                g if g > 0 => format!("через {g} дн."),
                g => format!("{} дн. назад", -g),
            };
            ui.label(RichText::new(format!("{} {}", sel.day(), OF_MONTH[sel.month0() as usize])).font(light(26.0)).color(FG));
            ui.label(RichText::new(format!("{} · {when}", DAYS[sel.weekday().num_days_from_monday() as usize])).color(DIM));
            // праздник выбранного дня: строка коралловым, список дел на неё короче — страница не растёт
            let fest = ru_holiday(sel.month(), sel.day());
            if let Some(name) = fest {
                ui.label(RichText::new(format!("\u{E734}  {name}")).font(semi(12.5)).color(Color32::from_rgb(0xFF, 0x9A, 0x8A)));
            }
            ui.add_space(6.0);
            let mine: Vec<&(usize, NaiveDate, String, String)> = dated.iter().filter(|d| d.1 == sel).collect();
            egui::ScrollArea::vertical().max_height(if fest.is_some() { 98.0 } else { 118.0 }).auto_shrink([false, true]).show(ui, |ui| {
                if mine.is_empty() {
                    ui.label(RichText::new(if sel < today { "В этот день дел не было." } else { "На этот день дел нет." }).color(DIM));
                }
                for (i, _, time, text) in mine {
                    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
                    glass_chip(ui.painter(), row, 10.0, 0.45);
                    let p = ui.painter().with_clip_rect(row.shrink2(vec2(0.0, 0.0)));
                    p.text(pos2(row.left() + 12.0, row.center().y), Align2::LEFT_CENTER, time, semi(13.0), accent());
                    p.with_clip_rect(Rect::from_min_max(pos2(row.left() + 58.0, row.top()), pos2(row.right() - 34.0, row.bottom())))
                        .text(pos2(row.left() + 58.0, row.center().y), Align2::LEFT_CENTER, text, FontId::proportional(13.0), FG);
                    let x = Rect::from_center_size(pos2(row.right() - 18.0, row.center().y), vec2(26.0, 26.0));
                    let resp = ui.interact(x, ui.id().with(("cal-del", *i)), Sense::click());
                    ui.painter().text(x.center(), Align2::CENTER_CENTER, "\u{E711}", FontId::proportional(12.0), if resp.hovered() { FG } else { DIM });
                    if resp.clicked() {
                        remove = Some(*i);
                    }
                    ui.add_space(3.0);
                }
            });
            if sel >= today {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 142.0).max(80.0);
                    let a = ui.add(egui::TextEdit::singleline(&mut self.cal_text).margin(egui::Margin::symmetric(9.0, 7.0)).hint_text(RichText::new("Новое дело").color(DIM)).desired_width(w));
                    let b = ui.add(egui::TextEdit::singleline(&mut self.cal_time).margin(egui::Margin::symmetric(9.0, 7.0)).hint_text(RichText::new("09:00").color(DIM)).desired_width(46.0));
                    let enter = (a.lost_focus() || b.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    add = jelly(ui, "+").on_hover_text("Напомнить в этот день").clicked() || enter;
                });
            }
        });

        if let Some(d) = click {
            self.cal_sel = Some(d);
        }
        if let Some(i) = remove {
            if i < self.cfg.reminders.len() {
                self.cfg.reminders.remove(i);
                self.cfg.save();
            }
        }
        if add {
            let text = self.cal_text.trim().to_string();
            let spec = self.cal_time.trim().replace(['.', ',', ' '], ":");
            let time = if spec.is_empty() { NaiveTime::from_hms_opt(9, 0, 0) } else { NaiveTime::parse_from_str(&spec, "%H:%M").ok() };
            let at = time.and_then(|t| resolve_here(sel.and_time(t))).map(|d| d.timestamp());
            match at {
                _ if text.is_empty() => self.notify("Впишите, о чём напомнить"),
                None => self.notify("Не понял время: напишите, например, 18:30"),
                Some(at) if at <= Local::now().timestamp() => self.notify("Это время уже прошло"),
                Some(at) => {
                    self.cfg.reminders.push(store::Reminder { text, at, repeat: 0 });
                    self.cfg.reminders.sort_by_key(|r| r.at);
                    self.cfg.save();
                    self.cal_text.clear();
                    self.cal_time.clear();
                    self.notify("Напоминание добавлено");
                }
            }
        }
    }

    /// Система → Монитор: четыре кольца — процессор, память, диск и батарея (или время работы).
    fn ui_system(&mut self, ui: &mut egui::Ui) {
        let s = self.stats.clone();
        let blue = Color32::from_rgb(0x8F, 0xD3, 0xFF);
        let green = Color32::from_rgb(0x7B, 0xE0, 0x9A);
        let (d, h, m) = (s.uptime / 86400, s.uptime % 86400 / 3600, s.uptime % 3600 / 60);
        // название, доля, крупное значение, пояснение, цвет, краснеть ли при переполнении
        let mut items: Vec<(&str, f32, String, String, Color32, bool)> = vec![("Процессор", s.cpu, format!("{:.0}%", s.cpu * 100.0), "загрузка сейчас".into(), blue, true)];
        if s.mem_total > 0.0 {
            let f = (s.mem_used / s.mem_total) as f32;
            items.push(("Память", f, format!("{:.0}%", f * 100.0), format!("{:.1} из {:.0} ГБ", s.mem_used, s.mem_total), accent(), true));
        }
        if s.disk_total > 0.0 {
            let f = (1.0 - s.disk_free / s.disk_total) as f32;
            items.push(("Диск C:", f, format!("{:.0}%", f * 100.0), format!("свободно {:.0} ГБ", s.disk_free), Color32::from_rgb(0xFF, 0xB3, 0x40), true));
        }
        match s.battery {
            Some((pct, plugged)) => items.push(("Батарея", pct as f32 / 100.0, format!("{pct}%"), if plugged { "от сети".into() } else { "от батареи".into() }, green, false)),
            None => items.push(("Без перезагрузки", (s.uptime as f32 / 604800.0).min(1.0), if d > 0 { format!("{d} д") } else { format!("{h} ч") }, format!("{d} д {h} ч {m} мин"), green, false)),
        }
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 132.0), Sense::hover());
        let n = items.len() as f32;
        let w = (row.width() - 8.0 * (n - 1.0)) / n;
        for (k, (name, frac, value, detail, color, alarm)) in items.into_iter().enumerate() {
            let r = Rect::from_min_size(pos2(row.left() + k as f32 * (w + 8.0), row.top()), vec2(w, row.height()));
            // карточки открывают нужное окно Windows: диск — «Память устройства», память и процессор — диспетчер задач
            let target = match name {
                "Диск C:" => Some(("ms-settings:storagesense", "Открыть «Память устройства» — там видно, чем занят диск, и можно освободить место")),
                "Процессор" | "Память" => Some(("taskmgr", "Открыть диспетчер задач")),
                _ => None,
            };
            let resp = ui.interact(r, ui.id().with(("sys-card", k)), if target.is_some() { Sense::click() } else { Sense::hover() });
            let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered() && target.is_some(), 0.12);
            glass_chip(ui.painter(), r, 18.0, 0.55 + 0.4 * hov);
            let shown = ui.ctx().animate_value_with_time(ui.id().with(("sys", k)), frac.clamp(0.0, 1.0), 0.5);
            let c = pos2(r.center().x, r.top() + 46.0);
            let hot = alarm && shown > 0.9;
            ring(ui.painter(), c, 30.0, 6.0, shown, if hot { Color32::from_rgb(0xFF, 0x6B, 0x6B) } else { color });
            ui.painter().text(c, Align2::CENTER_CENTER, value, semi(14.0), FG);
            ui.painter().text(pos2(c.x, r.top() + 96.0), Align2::CENTER_CENTER, name, semi(12.5), FG);
            // диск почти полон — вместо «свободно …» подсказываем, что делать
            let (detail, dc) = if hot && name == "Диск C:" { (format!("{detail} · освободить"), Color32::from_rgb(0xFF, 0x9A, 0x8A)) } else { (detail, DIM) };
            ui.painter().text(pos2(c.x, r.top() + 114.0), Align2::CENTER_CENTER, detail, FontId::proportional(11.5), dc);
            if let Some((cmd, tip)) = target {
                if resp.on_hover_text(tip).clicked() {
                    sys::open(cmd);
                }
            }
        }
        if s.battery.is_some() {
            ui.label(RichText::new(format!("Компьютер работает {d} д {h} ч {m} мин без перезагрузки")).color(DIM));
        }
        ui.add_space(4.0);

        // список программ обновляется раз в три секунды в отдельном потоке, чтобы панель не подтормаживала
        if self.procs_at.elapsed() > Duration::from_secs(3) {
            self.procs_at = Instant::now();
            let (procs, ctx) = (self.procs.clone(), ui.ctx().clone());
            let (procs_cpu, snap) = (self.procs_cpu.clone(), self.cpu_snap.clone());
            std::thread::spawn(move || {
                let list = sys::top_memory(5);
                *procs.lock().unwrap() = list;
                // процессор: разница времени процессов с прошлым снимком
                let (cur, at) = (sys::proc_times(), Instant::now());
                let prev = snap.lock().unwrap().replace((cur.clone(), at));
                if let Some((prev, was)) = prev {
                    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
                    let el = at.duration_since(was).as_nanos() as u64 / 100;
                    *procs_cpu.lock().unwrap() = sys::cpu_top(&prev, &cur, el, cores, 5);
                }
                ctx.request_repaint();
            });
        }
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 146.0), Sense::hover());
        let p = ui.painter().clone();
        let left = Rect::from_min_max(row.min, pos2(row.center().x - 4.0, row.bottom()));
        let right = Rect::from_min_max(pos2(row.center().x + 4.0, row.top()), row.max);

        // ----- нагрузка во времени
        glass_chip(&p, left, 18.0, 0.45);
        p.text(left.min + vec2(14.0, 10.0), Align2::LEFT_TOP, "Нагрузка", semi(12.5), FG);
        let secs = self.load_hist.len() as f32 * 1.5;
        let span = if secs >= 90.0 { format!("за {:.0} мин", secs / 60.0) } else { format!("за {secs:.0} с") };
        p.text(left.right_top() + vec2(-14.0, 11.0), Align2::RIGHT_TOP, span, FontId::proportional(11.0), DIM);
        let plot = Rect::from_min_max(left.min + vec2(14.0, 34.0), left.max - vec2(14.0, 26.0));
        for k in 0..=2 {
            let y = plot.bottom() - plot.height() * k as f32 / 2.0;
            p.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0_f32, rgba(255, 255, 255, 14)));
        }
        let n = self.load_hist.len();
        if n >= 2 {
            for (which, color) in [(1usize, accent()), (0, blue)] {
                // график всегда прижат к правому краю: новое появляется справа
                let pts: Vec<Pos2> = self
                    .load_hist
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let val = if which == 0 { v.0 } else { v.1 };
                        // пока замеров мало, график растянут шире: первые точки не жмутся к правому краю
                        let span = (n.max(30) - 1) as f32;
                        pos2(plot.right() - plot.width() * (n - 1 - i) as f32 / span.min(119.0), plot.bottom() - plot.height() * val.clamp(0.0, 1.0))
                    })
                    .collect();
                let mut mesh = egui::Mesh::default();
                for (k, pt) in pts.iter().enumerate() {
                    mesh.colored_vertex(*pt, Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 46));
                    mesh.colored_vertex(pos2(pt.x, plot.bottom()), Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 0));
                    if k > 0 {
                        let i = (k * 2) as u32;
                        mesh.add_triangle(i - 2, i - 1, i);
                        mesh.add_triangle(i - 1, i, i + 1);
                    }
                }
                p.add(egui::Shape::mesh(mesh));
                p.add(egui::Shape::line(pts, Stroke::new(1.8_f32, color)));
            }
        } else {
            p.text(plot.center(), Align2::CENTER_CENTER, "Собираю данные…", FontId::proportional(12.0), DIM);
        }
        let legend = pos2(left.left() + 14.0, left.bottom() - 13.0);
        p.circle_filled(legend + vec2(4.0, 0.0), 3.5, blue);
        p.text(legend + vec2(13.0, 0.0), Align2::LEFT_CENTER, "процессор", FontId::proportional(11.0), DIM);
        p.circle_filled(legend + vec2(92.0, 0.0), 3.5, accent());
        p.text(legend + vec2(101.0, 0.0), Align2::LEFT_CENTER, "память", FontId::proportional(11.0), DIM);

        // ----- кто занимает память
        glass_chip(&p, right, 18.0, 0.45);
        p.text(right.min + vec2(14.0, 10.0), Align2::LEFT_TOP, "Больше всего", semi(12.5), FG);
        // переключатель «память · процессор»: пилюля с ползущей подсветкой
        let seg = Rect::from_min_size(pos2(right.right() - 14.0 - 150.0, right.top() + 8.0), vec2(150.0, 20.0));
        p.rect_filled(seg, Rounding::same(10.0), rgba(255, 255, 255, 14));
        let k = ui.ctx().animate_bool_with_time(ui.id().with("procs-seg"), self.procs_by_cpu, 0.18);
        let half = seg.width() / 2.0;
        p.rect_filled(Rect::from_min_size(seg.min + vec2(half * k, 0.0), vec2(half, seg.height())), Rounding::same(10.0), rgba(255, 255, 255, 34));
        for (j, (label, by_cpu)) in [("памяти", false), ("процессора", true)].into_iter().enumerate() {
            let r = Rect::from_min_size(seg.min + vec2(half * j as f32, 0.0), vec2(half, seg.height()));
            let resp = ui.interact(r, ui.id().with(("procs-seg", j)), Sense::click());
            let on = self.procs_by_cpu == by_cpu;
            p.text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(11.0), if on { FG } else if resp.hovered() { FG.gamma_multiply(0.8) } else { DIM });
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !on {
                self.procs_by_cpu = by_cpu;
                self.kill_arm = None;
            }
        }
        // строки: имя, доля полоски, подпись справа, красная ли
        let total = (s.mem_total * 1024.0 * 1024.0 * 1024.0).max(1.0);
        let list: Vec<(String, f32, String, bool)> = if self.procs_by_cpu {
            let l = self.procs_cpu.lock().unwrap().clone();
            let top = l.first().map_or(1.0, |x| x.1).max(1e-6);
            l.into_iter().map(|(name, f)| (name, f / top, if f >= 0.095 { format!("{:.0}%", f * 100.0) } else { format!("{:.1}%", f * 100.0).replace('.', ",") }, f > 0.5)).collect()
        } else {
            let l = self.procs.lock().unwrap().clone();
            let top = l.first().map_or(1, |x| x.1).max(1) as f32;
            l.into_iter()
                .map(|(name, bytes)| {
                    let size = if bytes >= 1 << 30 { format!("{:.1} ГБ", bytes as f64 / (1u64 << 30) as f64).replace('.', ",") } else { format!("{} МБ", bytes >> 20) };
                    (name, bytes as f32 / top, size, bytes as f64 / total > 0.25)
                })
                .collect()
        };
        let mut kill: Option<String> = None;
        for (i, (name, frac, size, heavy)) in list.iter().enumerate() {
            let (frac, size, heavy) = (*frac, size.clone(), *heavy);
            let y = right.top() + 40.0 + i as f32 * 20.5;
            // строку можно нажать: первый щелчок взводит, второй закрывает программу
            let row_r = Rect::from_min_max(pos2(right.left() + 8.0, y - 10.0), pos2(right.right() - 8.0, y + 10.0));
            let resp = ui.interact(row_r, ui.id().with(("proc", i)), Sense::click());
            let safe = sys::can_kill(name);
            let armed = self.kill_arm.as_ref().is_some_and(|a| a.0 == *name && a.1 > Instant::now());
            let hot = safe && (resp.hovered() || armed);
            if hot {
                p.rect_filled(row_r, Rounding::same(7.0), if armed { rgba(255, 107, 107, 40) } else { rgba(255, 255, 255, 16) });
            }
            if resp.clicked() && safe {
                if armed {
                    kill = Some(name.clone());
                } else {
                    self.kill_arm = Some((name.clone(), Instant::now() + Duration::from_secs(4)));
                }
            }
            p.with_clip_rect(Rect::from_min_max(pos2(right.left() + 14.0, y - 10.0), pos2(right.left() + 124.0, y + 10.0)))
                .text(pos2(right.left() + 14.0, y), Align2::LEFT_CENTER, name, FontId::proportional(12.5), FG);
            let track = Rect::from_min_max(pos2(right.left() + 130.0, y - 2.5), pos2(right.right() - 74.0, y + 2.5));
            p.rect_filled(track, Rounding::same(2.5), rgba(255, 255, 255, 20));
            // красным — больше четверти всей памяти или больше половины процессора
            let fill = Rect::from_min_max(track.min, pos2(track.left() + track.width() * frac.clamp(0.03, 1.0), track.bottom()));
            p.rect_filled(fill, Rounding::same(2.5), if heavy { Color32::from_rgb(0xFF, 0x6B, 0x6B) } else { accent() });
            let red = Color32::from_rgb(0xFF, 0x8A, 0x80);
            let (label, color) = if armed { ("ещё раз".to_string(), red) } else if hot { ("закрыть".to_string(), FG) } else { (size, if heavy { red } else { DIM }) };
            p.text(pos2(right.right() - 14.0, y), Align2::RIGHT_CENTER, label, semi(12.0), color);
        }
        if let Some(name) = kill {
            self.kill_arm = None;
            let n = sys::kill_by_name(&name);
            self.notify(&if n > 0 { format!("{name} закрыта") } else { format!("{name}: закрыть не удалось") });
            self.procs_at = Instant::now().checked_sub(Duration::from_secs(2)).unwrap_or_else(Instant::now);
        }
        // скорость сети — в правом нижнем углу графика нагрузки
        let speed = |v: f32| if v >= 1048576.0 { format!("{:.1} МБ/с", v / 1048576.0).replace('.', ",") } else { format!("{:.0} КБ/с", v / 1024.0) };
        let net_at = pos2(left.right() - 14.0, left.bottom() - 13.0);
        let up = p.text(net_at, Align2::RIGHT_CENTER, format!("\u{E898} {}", speed(self.net.1)), FontId::proportional(11.0), DIM);
        p.text(pos2(up.left() - 10.0, net_at.y), Align2::RIGHT_CENTER, format!("\u{E896} {}", speed(self.net.0)), FontId::proportional(11.0), FG);
        if list.is_empty() {
            let msg = if self.procs_by_cpu && self.cpu_snap.lock().unwrap().is_some() { "Процессор свободен" } else { "Считаю…" };
            p.text(right.center(), Align2::CENTER_CENTER, msg, FontId::proportional(12.0), DIM);
        }
    }

    fn ui_palette(&mut self, ui: &mut egui::Ui) {
        let te = ui
            .allocate_ui_with_layout(vec2(ui.available_width(), 28.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add_space(2.0);
                ui.label(RichText::new("\u{E721}").size(15.0).color(accent()));
                ui.add_space(2.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.query)
                        .hint_text(RichText::new("Программа, сайт, пример или команда").color(DIM))
                        .desired_width(f32::INFINITY)
                        .frame(false)
                        .font(FontId::proportional(17.0)),
                )
            })
            .inner;
        if self.focus_query {
            te.request_focus();
            self.focus_query = false;
        }
        if te.changed() {
            self.build_results();
            self.sel = 0;
        }
        // демо-запрос строится до того, как в фоне загрузился список программ, — пересобираем один раз
        static DEMO_APPS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if self.palette_demo && !DEMO_APPS.load(Ordering::Relaxed) && !lock(&self.apps).is_empty() {
            DEMO_APPS.store(true, Ordering::Relaxed);
            self.build_results();
        }
        // спросили про валюту — подгружаем курсы и, когда они придут, пересчитываем ответ
        if convert::wants_rates(&self.query) {
            let ctx = ui.ctx().clone();
            self.rates.refresh(store::dir().join("rates.json"), move || ctx.request_repaint());
            let have = self.rates.get().is_some();
            if have != self.rates_shown {
                self.rates_shown = have;
                self.build_results();
            }
        }
        let (down, up, enter, esc) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if esc {
            self.mode = Mode::Pill;
            return;
        }
        let n = self.results.len();
        if n > 0 {
            if down {
                self.sel = (self.sel + 1) % n;
            }
            if up {
                self.sel = (self.sel + n - 1) % n;
            }
        }
        let mut run = None;
        if enter && n > 0 {
            run = Some(self.sel.min(n - 1));
        }
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.y = 2.0;
        if self.query.trim().is_empty() {
            // недавнее: последние запросы, по которым что-то запускали; щелчок подставляет
            let mut pick_recent: Option<String> = None;
            let mut forget = false;
            if !self.cfg.recent.is_empty() {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(RichText::new("\u{E81C}").size(12.0).color(DIM));
                    ui.label(RichText::new("Недавнее").size(11.5).color(DIM));
                    let room = ui.available_width() - 30.0;
                    let mut used = 0.0;
                    for q in &self.cfg.recent {
                        let w = ui.painter().layout_no_wrap(q.clone(), FontId::proportional(12.5), FG).size().x + 22.0;
                        if used + w > room {
                            break; // не помещается — остальные не показываем, чтобы ряд не переносился
                        }
                        used += w + 6.0;
                        let (r, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
                        let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
                        let p = ui.painter();
                        p.rect_filled(r, Rounding::same(13.0), mix(rgba(255, 255, 255, 10), accent().gamma_multiply(0.25), hov));
                        p.rect_stroke(r, Rounding::same(13.0), Stroke::new(1.0_f32, mix(rgba(255, 255, 255, 26), accent(), hov)));
                        p.text(r.center(), Align2::CENTER_CENTER, q, FontId::proportional(12.5), mix(DIM, FG, 0.6 + 0.4 * hov));
                        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            pick_recent = Some(q.clone());
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (r, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
                        let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
                        if hov > 0.0 {
                            ui.painter().circle_filled(r.center(), 11.0, rgba(255, 255, 255, (30.0 * hov) as u8));
                        }
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, "\u{E711}", FontId::proportional(9.5), mix(DIM, FG, hov));
                        if resp.on_hover_text("Забыть недавнее").clicked() {
                            forget = true;
                        }
                    });
                });
            }
            if forget {
                self.cfg.recent.clear();
                self.cfg.save();
            }
            if let Some(q) = pick_recent {
                self.query = q;
                self.build_results();
                self.sel = 0;
                self.focus_query = true;
            }
            // примеры: щелчок подставляет запрос
            let mut pick: Option<&str> = None;
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for (glyph, text) in [("\u{E916}", "таймер 5"), ("\u{E823}", "напомни через 15 мин чай"), ("\u{E73E}", "дело купить хлеб"), ("\u{E787}", "до нового года")] {
                    let label = format!("{glyph}  {}", text.trim());
                    let w = ui.painter().layout_no_wrap(label.clone(), FontId::proportional(12.5), FG).size().x + 20.0;
                    let (r, resp) = ui.allocate_exact_size(vec2(w, 28.0), Sense::click());
                    let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
                    glass_chip(ui.painter(), r, 14.0, 0.4 + 0.5 * hov);
                    ui.painter().text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(12.5), mix(DIM, FG, hov));
                    if resp.clicked() {
                        pick = Some(text);
                    }
                }
            });
            if let Some(t) = pick {
                self.query = t.to_string();
                self.build_results();
                self.sel = 0;
                self.focus_query = true;
            }
        }
        let icons = &mut self.app_icons;
        for (i, it) in self.results.iter().enumerate() {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H - 2.0), Sense::click());
            if resp.hovered() || i == self.sel {
                let a = if i == self.sel { 34 } else { 16 };
                ui.painter().rect_filled(r, Rounding::same(9.0), tint(a));
                if i == self.sel {
                    // выбранная строка — тонкая полоска акцента слева
                    let bar = Rect::from_min_max(pos2(r.left() + 3.0, r.top() + 8.0), pos2(r.left() + 5.5, r.bottom() - 8.0));
                    ui.painter().rect_filled(bar, Rounding::same(1.5), accent());
                }
            }
            // значок по типу результата; у программ — их собственный значок
            let ic = Rect::from_center_size(pos2(r.left() + 24.0, r.center().y), vec2(20.0, 20.0));
            let (glyph, color) = match &it.action {
                Action::Launch(_) => ("\u{E8FC}", Color32::from_rgb(0xC6, 0xCB, 0xDD)),
                Action::Url(_) if it.sub == "Google" => ("\u{E721}", Color32::from_rgb(0x8F, 0xCB, 0xFF)),
                Action::Url(_) => ("\u{E774}", Color32::from_rgb(0x7F, 0xC8, 0xFF)),
                Action::Copy(_) => ("\u{E8EF}", Color32::from_rgb(0xB9, 0xA4, 0xFF)),
                Action::Timer(_) => ("\u{E916}", Color32::from_rgb(0xFF, 0xB3, 0x40)),
                Action::Ask(_) => ("\u{E8BD}", Color32::from_rgb(0xE3, 0xA8, 0xFF)),
                Action::Quick(Quick::Todo(_)) => ("\u{E73E}", Color32::from_rgb(0x7B, 0xE0, 0x9A)),
                Action::Quick(Quick::Note(_)) => ("\u{E70B}", Color32::from_rgb(0xFF, 0xD0, 0x60)),
                Action::Quick(Quick::Remind(..)) => ("\u{E823}", Color32::from_rgb(0xFF, 0x8A, 0x7A)),
            };
            // значок грузится в фоне: ярлык на отключённый сетевой диск не должен подвешивать поиск
            let tex = match &it.action {
                Action::Launch(path) => icons
                    .entry(path.clone())
                    .or_insert_with(|| {
                        // один фоновый поток грузит значки по очереди: оболочка Windows не любит параллельных запросов
                        static WORKER: std::sync::OnceLock<Mutex<mpsc::Sender<String>>> = std::sync::OnceLock::new();
                        let worker = WORKER.get_or_init(|| {
                            let (req_tx, req_rx) = mpsc::channel::<String>();
                            let (tx, ctx) = (self.tx.clone(), ui.ctx().clone());
                            std::thread::spawn(move || {
                                unsafe {
                                    let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
                                }
                                for path in req_rx {
                                    let icon = sys::icon_rgba(&path);
                                    let _ = tx.send(Msg::Icon(path, icon));
                                    ctx.request_repaint();
                                }
                            });
                            Mutex::new(req_tx)
                        });
                        let _ = lock(worker).send(path.clone());
                        None
                    })
                    .clone(),
                _ => None,
            };
            match tex {
                Some(t) => rounded_image(ui.painter(), &t, ic, 0.0),
                None => {
                    ui.painter().circle_filled(ic.center(), 12.0, Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 26));
                    ui.painter().text(ic.center(), Align2::CENTER_CENTER, glyph, FontId::proportional(12.5), color);
                }
            }
            let p = ui.painter().with_clip_rect(r);
            p.text(pos2(r.right() - 12.0, r.center().y), Align2::RIGHT_CENTER, &it.sub, FontId::proportional(11.5), DIM);
            p.text(pos2(r.left() + 44.0, r.center().y), Align2::LEFT_CENTER, &it.title, FontId::proportional(14.0), FG);
            if resp.clicked() {
                run = Some(i);
            }
        }
        if let Some(i) = run {
            let action = self.results[i].action.clone();
            // cfg.save сам ничего не пишет в проверочном запуске
            // вставленное из буфера (там бывают пароли и личное) на диск не попадает
            let pasted = self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()).is_some_and(|t| t.trim() == self.query.trim());
            if recent_worthy(&action) && !pasted {
                let q = self.query.clone();
                push_recent(&mut self.cfg.recent, &q);
                self.cfg.save();
            }
            self.run(action);
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // штатное закрытие — не падение: откат при следующем запуске не нужен
        if !test_mode() {
            update::trial_passed();
        }
        store::save_notes(&self.notes);
        self.jarvis.stop();
        self.ears.stop_server();
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let now = Instant::now();
        let dt = frame_dt(ctx.input(|i| i.stable_dt));
        let time = ctx.input(|i| i.time);
        self.pump(ctx, now);
        let palette_open = self.mode == Mode::Palette;
        if self.palette_was_open && !palette_open {
            self.prune_icons();
        }
        self.palette_was_open = palette_open;
        // ── обновление как представление: шкала в пилюле → остров сжимается в каплю и втягивается за верхний
        // край экрана → новая версия падает оттуда же и разворачивается обратно
        self.upd_shape = None;
        let mut drop = 0.0f32;
        if let Some((at, version, path)) = self.upd_anim.clone() {
            let t = now.duration_since(at).as_secs_f32();
            ctx.request_repaint();
            if t < 1.9 {
                let a = accent();
                self.toast = Some(Toast {
                    glyph: "\u{E896}",
                    color: [a.r(), a.g(), a.b()],
                    title: format!("Обновление {version}"),
                    text: String::new(),
                    until: now + Duration::from_secs(1),
                    cover: false,
                    level: Some(ease((t / 1.7).min(1.0))),
                    compact: true,
                });
            } else {
                self.toast = None;
                self.upd_shape = Some(vec2(54.0, 30.0));
                // сначала капля чуть приседает, потом резко уходит вверх
                let k = ((t - 2.15) / 0.40).clamp(0.0, 1.0);
                let dip = (((t - 1.9) / 0.25).clamp(0.0, 1.0) * std::f32::consts::PI).sin() * 5.0;
                drop = dip - 90.0 * k * k * k;
            }
            if t >= 2.6 {
                self.upd_anim = None;
                if path.as_os_str().is_empty() {
                    // проверочный прогон без установки: сразу показываем возвращение
                    self.arrive_at = Some(now);
                } else {
                    match update::apply(&path) {
                        Ok(()) => {
                            store::save_notes(&self.notes);
                            self.jarvis.stop();
                            self.ears.stop_server();
                            std::process::exit(0);
                        }
                        Err(e) => {
                            *self.upd.lock().unwrap() = update::State::Failed(e);
                            self.upd_at = now + Duration::from_secs(600);
                            self.arrive_at = Some(now);
                        }
                    }
                }
            }
        }
        if let Some(at) = self.arrive_at {
            let t = now.duration_since(at).as_secs_f32();
            ctx.request_repaint();
            if t < 1.0 {
                // падение из-за верхнего края и один мягкий отскок
                let k = (t / 0.42).min(1.0);
                let fall = -90.0 * (1.0 - k) * (1.0 - k);
                let bounce = if t > 0.42 { -7.0 * ((t - 0.42) / 0.30).min(1.0).mul_add(std::f32::consts::PI, 0.0).sin() * (1.0 - ((t - 0.42) / 0.58).min(1.0)) } else { 0.0 };
                drop = fall + bounce;
                if t < 0.40 {
                    self.upd_shape = Some(vec2(54.0, 30.0));
                } else if !self.arrive_done {
                    // капля приземлилась — расплёскивается в пилюлю
                    self.arrive_done = true;
                    self.ripple = 6.0;
                    self.jelly.vel += 1.6;
                    self.sheen_at = now;
                }
            } else {
                self.arrive_at = None;
                self.arrive_done = false;
            }
        }
        self.drop_y = drop;

        self.web_slot = None;
        let dropped: Vec<String> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.as_ref().map(|p| p.display().to_string())).collect());
        if !dropped.is_empty() {
            // app_icons не чистим: ключи общие с ярлыками палитры/запуска
            for path in dropped {
                self.shelf_meta.remove(&path);
                self.shelf_thumbs.remove(&path);
                shelf_add(&mut self.cfg.shelf, path);
            }
            let shelf = &self.cfg.shelf;
            self.shelf_meta.retain(|k, _| shelf.contains(k));
            self.shelf_thumbs.retain(|k, _| shelf.contains(k));
            self.cfg.save();
            self.mode = Mode::Panel;
            self.tab = Tab::Shelf;
            self.sticky = true;
        }

        // снимок всего экрана: пару кадров остров не рисуется, чтобы не попасть в кадр
        if let Some(n) = self.shot {
            ctx.request_repaint();
            if n > 0 {
                self.shot = Some(n - 1);
            } else {
                self.shot = None;
                let res = sys::capture_screen().and_then(|(w, h, px)| {
                    let path = sys::save_png(w, h, &px)?;
                    Ok((w, h, px, path))
                });
                match res {
                    Ok((w, h, px, path)) => {
                        if let Some(cb) = &mut self.clipboard {
                            let _ = cb.set_image(arboard::ImageData { width: w, height: h, bytes: px.clone().into() });
                        }
                        self.show_shot(ctx, w, h, px, path);
                    }
                    Err(e) => self.notify(&format!("Ошибка: {e}")),
                }
            }
        }

        // Остров живёт в координатах «поля» шириной WIN_W вверху основного экрана,
        // а настоящее окно подгоняется под его размер: системное размытие ложится только под остров.
        let ppp = ctx.pixels_per_point();
        let base = pos2(base_x(ppp), 0.0);
        let cursor: Option<Pos2> = sys::cursor().map(|(x, y)| pos2(x as f32 / ppp - base.x, y as f32 / ppp - base.y));
        let home = pos2(WIN_W / 2.0, TOP + PILL_H / 2.0);
        let d = cursor.map(|c| c - home).unwrap_or(vec2(9999.0, 9999.0));
        let dist = d.length();

        if self.mode != self.mode_seen {
            self.mode_seen = self.mode;
            self.mode_at = now;
            self.ripple = (self.ripple + 3.0).min(7.0);
            self.jelly.vel += 0.8;
            self.splash(0.5, 70.0);
            self.tab_at = now;
            self.sheen_at = now;
        }
        let mode_t = ease(now.duration_since(self.mode_at).as_secs_f32() / 0.20);
        let segs = self.segments();
        let target = match self.mode {
            Mode::Pill => match &self.toast {
                Some(t) => {
                    let w = |text: &str, font: FontId| ctx.fonts(|f| f.layout_no_wrap(text.to_string(), font, FG).size().x);
                    if t.compact {
                        vec2(if t.level.is_some() { 236.0 + if t.title.is_empty() { 0.0 } else { w(&t.title, semi(13.0)) + 10.0 } } else { (62.0 + w(&t.title, semi(13.5))).max(120.0) }, 40.0)
                    } else {
                        vec2((66.0 + w(&t.title, semi(13.5)).max(w(&t.text, FontId::proportional(12.0))) + 24.0).clamp(220.0, 470.0), 54.0)
                    }
                }
                None => vec2(self.pill_width(ctx, &segs), PILL_H),
            },
            Mode::Panel => vec2(PANEL_W, self.panel_height()),
            Mode::Palette => {
                let n = self.results.len() as f32;
                // пустой запрос — под полем ряд примеров
                // и, если есть, ряд «Недавнее» над ним (26 px + отступы)
                let recent = if self.cfg.recent.is_empty() { 0.0 } else { 34.0 };
                let hints = if self.query.trim().is_empty() { 40.0 + recent } else { 0.0 };
                vec2(PANEL_W, 50.0 + n * ROW_H + if n > 0.0 { 10.0 } else { 0.0 } + hints)
            }
        };
        let target = self.upd_shape.unwrap_or(target);
        // пружина: остров слегка «переливается» за цель и возвращается, как капля
        let sdt = dt.min(0.02);
        self.vel += ((target - self.size) * 340.0 - self.vel * 31.0) * sdt;
        self.size += self.vel * sdt;
        self.size = self.size.max(vec2(40.0, 20.0));
        let animating = (target - self.size).length() > 0.4 || self.vel.length() > 3.0;

        let rect_v = Rect::from_min_size(pos2(WIN_W / 2.0 - self.size.x / 2.0, TOP + self.drop_y), self.size);
        let layout_v = Rect::from_min_size(pos2(WIN_W / 2.0 - target.x / 2.0, TOP + self.drop_y), target);

        // «жидкая» физика (по образцу Atoll): пилюля тянется к курсору, пружинит и идёт рябью
        let (mut bump, mut lean_t) = (0.0f32, 0.0f32);
        if self.mode == Mode::Pill && !self.hidden && self.fx.is_none() {
            if let Some(c) = cursor {
                let dx = if c.x < rect_v.left() { c.x - rect_v.left() } else if c.x > rect_v.right() { c.x - rect_v.right() } else { 0.0 };
                let dy = if c.y > rect_v.bottom() { c.y - rect_v.bottom() } else if c.y < rect_v.top() { c.y - rect_v.top() } else { 0.0 };
                let dist = (dx * dx + dy * dy).sqrt();
                if dist > 0.0 && dist < 170.0 {
                    let k = 1.0 - dist / 170.0;
                    let k = k * k * (3.0 - 2.0 * k);
                    bump = k;
                    if dx != 0.0 {
                        lean_t = dx.signum() * 4.0 * k;
                    }
                    self.reach_cursor += (c - self.reach_cursor) * 0.35;
                }
            }
        }
        self.reach.target = bump;
        self.lean_s.target = lean_t;
        self.reach.step(dt);
        self.lean_s.step(dt);
        self.jelly.step(dt);
        let side = self.fx.is_some() && self.mode == Mode::Pill && self.toast.is_none() && !self.hidden && self.upd_anim.is_none() && self.arrive_at.is_none();
        self.bub_timer.target = if side && self.timer_end.is_some() { 1.0 } else { 0.0 };
        self.bub_call.target = if side && self.call_since.is_some() { 1.0 } else { 0.0 };
        self.bub_timer.step(dt);
        self.bub_call.step(dt);
        self.bub_clip.target = if side && self.hint.is_some() && now < self.hint_until { 1.0 } else { 0.0 };
        self.bub_clip.step(dt);
        let dl_now = *self.dl.lock().unwrap();
        self.bub_dl.target = if side && dl_now.is_some() { 1.0 } else { 0.0 };
        self.bub_dl.step(dt);
        self.ripple *= (-dt * 2.3).exp();
        if self.ripple < 0.03 {
            self.ripple = 0.0;
        }
        self.ripple_phase = wrap_phase(self.ripple_phase + dt * 7.5, RIPPLE_PERIOD);
        self.drip_phase = wrap_phase(self.drip_phase + dt * 2.6, DRIP_PERIOD);
        // остров-колба: жидкость внутри пилюли. Уровень что-то значит, поверхность живая.
        let flask = self.flask_on();
        let mut sloshing = false;
        if flask && !self.liq_h.is_empty() && self.liq_v.len() == self.liq_h.len() {
            let n = self.liq_h.len();
            if let Some(c) = cursor {
                // курсор рядом баламутит поверхность, как палец у воды
                let vx = c.x - self.last_cx;
                self.last_cx = c.x;
                let dy = (c.y - rect_v.center().y).abs();
                if c.x > rect_v.left() && c.x < rect_v.right() && dy < 70.0 && vx.abs() > 0.5 {
                    let i = (((c.x - rect_v.left()) / rect_v.width()) * (n as f32 - 1.0)) as usize;
                    self.liq_v[i.min(n - 1)] -= (vx.abs() * 0.9).min(14.0) * (1.0 - dy / 70.0) * 9.0;
                }
            }
            if self.media_now.as_ref().is_some_and(|m| m.playing) {
                // музыка: мелкие толчки в разных местах, как от баса
                let beat = (self.drip_phase * 2.6).sin();
                if beat > 0.985 {
                    let i = ((self.drip_phase * 37.0).sin() * 0.5 + 0.5) * (n as f32 - 1.0);
                    self.liq_v[i as usize] -= 26.0;
                }
            }
            let steps = (dt / 0.008).ceil().max(1.0) as usize;
            let hs = dt / steps as f32;
            for _ in 0..steps {
                for i in 0..n {
                    let left = self.liq_h[i.saturating_sub(1)];
                    let right = self.liq_h[(i + 1).min(n - 1)];
                    let acc = -30.0 * self.liq_h[i] - 3.2 * self.liq_v[i] + 900.0 * (left + right - 2.0 * self.liq_h[i]);
                    self.liq_v[i] += acc * hs;
                }
                for i in 0..n {
                    self.liq_h[i] = (self.liq_h[i] + self.liq_v[i] * hs).clamp(-9.0, 9.0);
                }
            }
            sloshing = self.liq_h.iter().any(|h| h.abs() > 0.06) || self.liq_v.iter().any(|v| v.abs() > 0.5);
            // уровень: таймер убывает до нуля, трек наполняет колбу, в покое — занятая память
            let fill = if let Some(end) = self.timer_end {
                end.saturating_duration_since(now).as_secs_f32() / self.timer_total.max(1.0)
            } else if let Some(m) = self.media_now.as_ref().filter(|m| m.dur > 0.0) {
                ((if m.playing { m.pos + m.at.elapsed().as_secs_f64() } else { m.pos }) / m.dur) as f32
            } else if self.stats.mem_total > 0.0 {
                (self.stats.mem_used / self.stats.mem_total) as f32
            } else {
                0.35
            };
            let target = 0.10 + 0.80 * fill.clamp(0.0, 1.0);
            if (target - self.level).abs() > 0.003 {
                self.level += (target - self.level) * (1.0 - (-dt * 3.0).exp());
                sloshing = true;
            }
        }
        let dripping = !flask && self.mode == Mode::Pill && self.media_now.as_ref().is_some_and(|m| m.playing);
        let glowing_music = self.album.is_some() && self.media_now.as_ref().is_some_and(|m| m.playing);
        let glowing = self.glow.is_some_and(|g| g.1.elapsed().as_secs_f32() < 1.3);
        let moving = !self.reach.settled() || self.reach.v > 0.01 || !self.lean_s.settled() || !self.jelly.settled() || self.ripple > 0.0 || dripping || glowing || glowing_music || self.sound.active() || sloshing || !self.bub_timer.settled() || !self.bub_call.settled() || !self.bub_clip.settled() || !self.bub_dl.settled() || self.sheen_at.elapsed().as_secs_f32() < 0.75;
        let animating = animating || moving;
        let scaled = rect_v.size() * self.jelly.v;
        let rect_v = Rect::from_min_size(pos2(WIN_W / 2.0 - scaled.x / 2.0 + self.lean_s.v, TOP + self.drop_y), scaled);
        // для снимков страницы проекта: тестовая копия пишет, где сейчас корпус острова (в пикселях экрана)
        if test_mode() {
            if let Some(path) = std::env::var_os("OSTROVOK_RECT_LOG") {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
                    let _ = writeln!(f, "{ms} {:.1} {:.1} {:.1} {:.1}", (base.x + rect_v.left()) * ppp, (base.y + rect_v.top()) * ppp, rect_v.width() * ppp, rect_v.height() * ppp);
                }
            }
        }

        // капли справа от пилюли: выезжают из неё и втягиваются обратно, по пути срастаясь «шейкой»
        let mut bubbles_v: Vec<(Rect, f32, u8, String)> = Vec::new();
        {
            let clock = |secs: u64| format!("{:02}:{:02}", secs / 60, secs % 60);
            let timer_text = self.timer_end.map(fmt_left).unwrap_or_else(|| "00:00".into());
            let call_text = clock(self.call_since.map_or(0, |s| s.elapsed().as_secs()));
            let mut edge = rect_v.right();
            let hint_text = self.hint.as_ref().map(|h| h.label.clone()).unwrap_or_default();
            let dl_text = match dl_now {
                Some((bytes, speed)) => {
                    let size = if bytes >= 1 << 30 { format!("{:.1} ГБ", bytes as f64 / (1u64 << 30) as f64) } else { format!("{} МБ", (bytes >> 20).max(1)) };
                    let rate = if speed >= 1048576.0 { format!("{:.1} МБ/с", speed / 1048576.0) } else { format!("{:.0} КБ/с", speed / 1024.0) };
                    format!("{size} · {rate}").replace('.', ",")
                }
                None => std::mem::take(&mut self.dl_text),
            };
            if dl_now.is_some() {
                self.dl_text = dl_text.clone();
            }
            for (kind, sv, text) in [(0u8, self.bub_timer.v, timer_text), (1u8, self.bub_call.v, call_text), (2u8, self.bub_clip.v, hint_text), (3u8, self.bub_dl.v, dl_text)] {
                if sv > 0.02 {
                    let w = 44.0 + ctx.fonts(|f| f.layout_no_wrap(text.clone(), semi(13.0), FG).size().x);
                    let r = Rect::from_min_size(pos2(edge - w + (w + 9.0) * sv.min(1.15), rect_v.center().y - 15.0), vec2(w, 30.0));
                    edge = r.right();
                    bubbles_v.push((r, sv, kind, text));
                }
            }
        }

        let over = cursor.is_some_and(|c| rect_v.expand(8.0).contains(c) || bubbles_v.iter().any(|b| b.0.contains(c)));
        let invisible = self.hidden || self.shot.is_some();

        // стекло: фон под окном размывает сама Windows (экран программа не снимает)
        if self.acrylic.is_none() && time > 0.15 {
            // OSTROVOK_BLUR выбирает способ размытия: acrylic, blur или off
            let tint = Some((18, 20, 28, 1));
            self.acrylic = Some(match std::env::var("OSTROVOK_BLUR").as_deref().unwrap_or(DEFAULT_BLUR) {
                "acrylic" => window_vibrancy::apply_acrylic(&*frame, tint).is_ok(),
                "blur" => window_vibrancy::apply_blur(&*frame, tint).is_ok(),
                _ => false,
            });
        }

        // настоящее окно: по размеру острова; на время анимации — по большему из двух размеров
        let mut span = if animating { rect_v.union(layout_v) } else { layout_v };
        for b in &bubbles_v {
            span = span.union(b.0);
        }
        let grown = span.expand(46.0);
        let win_v = Rect::from_min_max(grown.min.floor(), grown.max.ceil());
        let win_pos = if invisible { pos2(-9000.0, 0.0) } else { base + win_v.min.to_vec2() };
        let want = [win_pos.x, win_pos.y, win_v.width(), win_v.height()];
        if self.win_last != Some(want) {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(win_v.size()));
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(win_pos));
            self.win_last = Some(want);
        }
        let pass = !over || invisible;
        if self.passthrough != Some(pass) {
            ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(pass));
            self.passthrough = Some(pass);
        }
        // жидкое стекло: раз в ~0,7 с (и сразу при смене размера) берём снимок экрана под островом
        // Стекло видит фон, только пока остров скрыт от захвата экрана. Если остров должен быть виден
        // на демонстрации (галочка в настройках или идёт звонок в Discord), стекло сменяется плотным корпусом.
        if time > 0.2 {
            let off = std::env::var_os("OSTROVOK_GLASS").is_some_and(|v| v == "off");
            let hide = !off && !self.cfg.capture_visible && self.call_since.is_none();
            if self.capture_req != Some(hide) {
                if let Some(hwnd) = hwnd_of(frame) {
                    self.capture_req = Some(hide);
                    self.excluded = Some(sys::set_capture_hidden(hwnd, hide) && hide);
                    if !hide {
                        self.lens_cap = None;
                        self.lens_busy = false;
                    }
                }
            }
        }
        while let Ok(f) = self.lens_rx.try_recv() {
            self.lens_busy = false;
            let img = egui::ColorImage::from_rgba_unmultiplied([f.w, f.h], &f.rgba);
            match &mut self.lens_tex {
                Some(tex) => tex.set(img, egui::TextureOptions::LINEAR),
                None => self.lens_tex = Some(ctx.load_texture("lens", img, egui::TextureOptions::LINEAR)),
            }
            self.lens_cap = Some(Rect::from_min_max(pos2(f.cap[0], f.cap[1]), pos2(f.cap[2], f.cap[3])));
        }
        if self.excluded == Some(true) && !invisible {
            const MARGIN: f32 = 28.0;
            let top = (layout_v.top() - MARGIN).max(0.0);
            let cap = Rect::from_min_max(pos2(layout_v.left() - MARGIN, top), layout_v.max + vec2(MARGIN, MARGIN));
            let px = |v: f32| (v * ppp).round() as i32;
            let radius = if self.mode == Mode::Pill { PILL_H / 2.0 } else { 22.0 }.min(layout_v.height() / 2.0);
            let key = [px(base.x + cap.left()), px(base.y + cap.top()), px(cap.width()), px(cap.height()), px(radius)];
            let due = key != self.lens_key || now.duration_since(self.lens_at) >= Duration::from_millis(700);
            if due && !self.lens_busy {
                self.lens_key = key;
                self.lens_at = now;
                self.lens_busy = true;
                let _ = self.lens_tx.send(glass::Req {
                    x: key[0],
                    y: key[1],
                    w: key[2],
                    h: key[3],
                    ix: ((layout_v.left() - cap.left()) * ppp) as f64,
                    iy: ((layout_v.top() - cap.top()) * ppp) as f64,
                    iw: (layout_v.width() * ppp) as f64,
                    ih: (layout_v.height() * ppp) as f64,
                    rr: (radius * ppp) as f64,
                    cap: [cap.left(), cap.top(), cap.right(), cap.bottom()],
                    soft: if self.mode == Mode::Pill { 3 } else { (13.0 * ppp) as usize },
                });
            }
        }

        // рисуем относительно того места, где окно находится на самом деле в этом кадре
        let origin = match ctx.input(|i| i.viewport().inner_rect) {
            Some(actual) if !invisible => actual.min - base.to_vec2(),
            _ => win_v.min,
        };
        let rect = rect_v.translate(-origin.to_vec2());
        let layout = layout_v.translate(-origin.to_vec2());
        self.pill_rect = rect;
        self.bubbles = bubbles_v.into_iter().map(|(r, sv, kind, text)| (r.translate(-origin.to_vec2()), sv, kind, text)).collect();
        let cursor_win = cursor.map(|c| c - origin.to_vec2());
        let playing_now = self.media_now.as_ref().is_some_and(|m| m.playing);
        self.warp = Warp {
            reach: self.reach.v,
            cursor: self.reach_cursor - origin.to_vec2(),
            ripple: self.ripple,
            phase: self.ripple_phase,
            drip: if dripping { 1.6 } else { 0.0 },
            drip_phase: self.drip_phase,
        };
        self.lens_draw = match (self.excluded, self.lens_cap) {
            (Some(true), Some(cap)) if self.lens_tex.is_some() => Some(cap.translate(-origin.to_vec2())),
            _ => None,
        };

        // карточка снимка висит несколько секунд и уходит сама, если к ней не подошли
        if self.mode == Mode::Panel && self.tab == Tab::Shot && self.sticky && self.shot_card.as_ref().is_none_or(|c| now > c.until) && std::env::var_os("OSTROVOK_DEMO").is_none() {
            self.mode = Mode::Pill;
            self.sticky = false;
        }
        // панель сворачивается, когда курсор ушёл
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        match self.mode {
            Mode::Panel => {
                let fg = sys::we_are_foreground();
                if self.was_foreground && !fg && !over && !self.sticky && std::env::var_os("OSTROVOK_DEMO").is_none() {
                    self.mode = Mode::Pill;
                }
                self.was_foreground = fg;
                if over {
                    self.outside_since = None;
                    self.sticky = false;
                } else if !self.sticky {
                    let since = *self.outside_since.get_or_insert(now);
                    let typing = (ctx.memory(|m| m.focused().is_some()) && focused) || (self.web_visible && sys::we_are_foreground());
                    if now.duration_since(since) > Duration::from_millis(900) && !typing && std::env::var_os("OSTROVOK_DEMO").is_none() {
                        self.mode = Mode::Pill;
                    }
                }
            }
            Mode::Palette => {
                if !focused && now.duration_since(self.palette_at) > Duration::from_millis(700) && !self.palette_demo {
                    self.mode = Mode::Pill;
                }
            }
            Mode::Pill => {
                self.outside_since = None;
                self.was_foreground = false;
            }
        }

        egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
            if invisible {
                return;
            }
            let radius = if self.mode == Mode::Pill { PILL_H / 2.0 } else { 22.0 }.min(rect.height() / 2.0);
            let to_cursor = d - (rect_v.center() - home);
            let light = if dist < 700.0 && to_cursor.length() > 1.0 { to_cursor.normalized() } else { vec2(-0.5, -0.86) };
            let dim = 255;
            match self.fx.clone() {
                Some(fx) => {
                    // форма: пилюля и капли рядом с ней; видеокарта сама срастит их
                    let mut blobs = [[0.0f32; 4]; fx::MAX_BLOBS];
                    let mut radii = [0.0f32; fx::MAX_BLOBS];
                    // на ходу корпус чуть сплющивается, как тянущаяся резина
                    let squash = (self.vel.x.abs() / 420.0).min(3.2).min(rect.height() * 0.12);
                    blobs[0] = [rect.center().x, rect.center().y, rect.width() / 2.0, rect.height() / 2.0 - squash];
                    radii[0] = radius;
                    let mut count = 1;
                    for (r, _, _, _) in self.bubbles.iter().take(fx::MAX_BLOBS - 1) {
                        blobs[count] = [r.center().x, r.center().y, r.width() / 2.0, r.height() / 2.0];
                        radii[count] = 15.0;
                        count += 1;
                    }
                    let unit = |c: [u8; 3], a: f32| [c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0, a];
                    let glow = match (self.glow, self.album.filter(|_| playing_now)) {
                        (Some((c, at)), _) if at.elapsed().as_secs_f32() < 1.3 => unit([c.r(), c.g(), c.b()], 0.9 * (1.0 - at.elapsed().as_secs_f32() / 1.3).powi(3)),
                        (_, Some(c)) => unit(c, 0.30 + 0.12 * (self.drip_phase * 0.7).sin() + 0.28 * self.beat),
                        _ => [0.0; 4],
                    };
                    let light_at = cursor_win.unwrap_or(rect.center() + vec2(-200.0, -300.0));
                    let params = fx::Params {
                        blobs,
                        radius: radii,
                        count: count as i32,
                        merge: 14.0,
                        time: time as f32,
                        ripple: self.ripple,
                        light: [light_at.x, light_at.y],
                        cap: self.lens_draw.map_or([0.0; 4], |c| [c.left(), c.top(), c.right(), c.bottom()]),
                        has_tex: self.lens_draw.is_some(),
                        // у открытой панели стекло светлее самого тёмного фона: на чёрном окне она остаётся видна
                        tint: {
                            let lift = self.mode != Mode::Pill && self.lens_draw.is_some();
                            theme().glass.map(|c| if lift { c as f32 / 255.0 * 2.3 + 0.04 } else { c as f32 / 255.0 })
                        },
                        // со снимком фона — доля затемнения; без него (остров виден на записи) — плотность корпуса:
                        // сквозь него просвечивает сам экран
                        dim: {
                            // t — прозрачность из настроек, 0…1
                            let t = self.clearness() as f32 / 100.0;
                            match (self.lens_draw.is_some(), self.mode == Mode::Pill) {
                                (true, true) => 0.62 - 0.45 * t,
                                // открытая панель плотнее пилюли: на ней текст, он не должен теряться на фоне
                                (true, false) => 0.95 - 0.15 * t,
                                (false, true) => 1.0 - 0.45 * t,
                                // без снимка фона размытия нет вовсе, поэтому панель почти непрозрачная
                                (false, false) => 1.0 - 0.04 * t,
                            }
                        },
                        glow,
                        thick: (rect.width().min(rect.height()) / 2.0 * 0.92).clamp(4.0, 17.0),
                        sheen: self.sheen_at.elapsed().as_secs_f32() / 0.7,
                        rim: if self.mode == Mode::Pill { 0.07 } else { 0.20 },
                    };
                    let tex_id = self.lens_tex.as_ref().map(|t| t.id());
                    let _ = (light, dim);
                    ui.painter().add(egui::PaintCallback {
                        rect: ui.max_rect(),
                        callback: Arc::new(eframe::egui_glow::CallbackFn::new(move |info, painter| {
                            let vp = info.viewport_in_pixels();
                            let tex = tex_id.and_then(|id| painter.texture(id));
                            fx.paint(painter.gl(), tex, vp.left_px as f32, (vp.from_bottom_px + vp.height_px) as f32, info.pixels_per_point, &params);
                        })),
                    });
                }
                None => self.draw_glass(ui.painter(), rect, radius, light, dim),
            }

            let look = vec2((d.x / 45.0).clamp(-2.6, 2.6), (d.y / 45.0).clamp(-2.2, 2.2));
            let face_at = rect.min + if self.mode == Mode::Pill { vec2(12.0, 9.0) } else { vec2(22.0, 17.0) };

            match self.mode {
                Mode::Pill => {
                    let island = ui.interact(rect, Id::new("island"), Sense::click());
                    let face = ui.interact(Rect::from_min_size(face_at, vec2(38.0, 18.0)).expand(4.0), Id::new("face"), Sense::click());
                    // «под курсором» считается по самой пилюле, а не по виджету: кнопки плеера лежат поверх
                    // и иначе перехватывали бы наведение — пилюля мигала бы между названием и кнопками
                    let reach = if self.hover_was { 7.0 } else { 0.0 };
                    let hot = ui.ctx().pointer_hover_pos().is_some_and(|c| rect.expand(reach).contains(c));
                    self.jelly.target = if island.is_pointer_button_down_on() || face.is_pointer_button_down_on() { 0.94 } else if hot { 1.045 } else { 1.0 };
                    if hot && !self.hover_was {
                        self.sheen_at = now;
                    }
                    self.hover_was = hot;
                    // колесо мыши над островом меняет общую громкость
                    let wheel = if hot { ui.input(|i| i.raw_scroll_delta.y) } else { 0.0 };
                    if wheel != 0.0 {
                        if let Some((level, _)) = sys::volume() {
                            let v = (level + wheel.signum() * 0.02).clamp(0.0, 1.0);
                            sys::set_volume(v);
                        }
                    }
                    if face.clicked() {
                        self.next_emotion(now);
                        self.splash(0.12, 90.0);
                        self.jelly.vel += 1.4;
                        self.ripple = (self.ripple + 2.5).min(7.0);
                    } else if island.clicked() {
                        self.mode = Mode::Panel;
                    }
                    match &self.toast {
                        Some(t) if t.compact => {
                            let p = ui.painter().with_clip_rect(rect);
                            let cy = rect.center().y;
                            let tone = Color32::from_rgb(t.color[0], t.color[1], t.color[2]);
                            p.text(pos2(rect.left() + 24.0, cy), Align2::CENTER_CENTER, t.glyph, FontId::proportional(16.0), tone);
                            match t.level {
                                Some(level) => {
                                    let mut x0 = rect.left() + 46.0;
                                    if !t.title.is_empty() {
                                        x0 = p.text(pos2(x0 - 2.0, cy), Align2::LEFT_CENTER, &t.title, semi(13.0), FG).right() + 12.0;
                                    }
                                    let track = Rect::from_min_max(pos2(x0, cy - 3.0), pos2(rect.right() - 54.0, cy + 3.0));
                                    p.rect_filled(track, Rounding::same(3.0), rgba(255, 255, 255, 34));
                                    let shown = ui.ctx().animate_value_with_time(Id::new("hud-level"), level.clamp(0.0, 1.0), 0.10);
                                    if shown > 0.005 {
                                        p.rect_filled(Rect::from_min_max(track.min, pos2(track.left() + (track.width() * shown).max(6.0), track.bottom())), Rounding::same(3.0), tone);
                                    }
                                    p.text(pos2(rect.right() - 18.0, cy), Align2::RIGHT_CENTER, format!("{:.0}", level * 100.0), semi(13.5), FG);
                                }
                                None => {
                                    p.text(pos2(rect.left() + 44.0, cy), Align2::LEFT_CENTER, &t.title, semi(13.5), FG);
                                }
                            }
                        }
                        Some(t) => {
                            let p = ui.painter().with_clip_rect(rect);
                            let at = pos2(rect.left() + 29.0, rect.center().y);
                            match (&self.thumb, t.cover) {
                                (Some(tex), true) => rounded_image(&p, tex, Rect::from_center_size(at, vec2(36.0, 36.0)), 10.0),
                                _ => {
                                    let dark = [t.color[0] / 3 * 2, t.color[1] / 3 * 2, t.color[2] / 3 * 2];
                                    icon_disc(&p, at, 18.0, t.color, dark, t.glyph, 16.0);
                                }
                            }
                            let lone = t.text.is_empty();
                            p.text(pos2(rect.left() + 58.0, rect.center().y - if lone { 0.0 } else { 9.0 }), Align2::LEFT_CENTER, &t.title, semi(13.5), FG);
                            if !lone {
                                p.text(pos2(rect.left() + 58.0, rect.center().y + 10.0), Align2::LEFT_CENTER, &t.text, FontId::proportional(12.0), DIM);
                            }
                        }
                        None => {
                            self.draw_pill(ui, rect, &segs, time);
                            self.draw_face(ui.painter(), face_at, look, now);
                        }
                    }
                    // содержимое капель проявляется, когда капля уже отделилась
                    let (mut open_timer, mut run_hint, mut open_dl) = (false, false, false);
                    for (i, (r, sv, kind, text)) in self.bubbles.iter().enumerate() {
                        let a = ((sv - 0.55) / 0.45).clamp(0.0, 1.0);
                        if a <= 0.0 {
                            continue;
                        }
                        let (glyph, color) = match (*kind, &self.hint) {
                            (0, _) => ("\u{E916}", Color32::from_rgb(0xFF, 0xB3, 0x40)),
                            (2, Some(h)) => (h.glyph, Color32::from_rgb(h.color[0], h.color[1], h.color[2])),
                            (3, _) => ("\u{E896}", Color32::from_rgb(0x5A, 0xAB, 0xFF)),
                            _ => ("\u{E720}", Color32::from_rgb(0x9A, 0xA3, 0xFF)),
                        };
                        let fade = |c: Color32| rgba(c.r(), c.g(), c.b(), (a * 255.0) as u8);
                        ui.painter().text(pos2(r.left() + 18.0, r.center().y), Align2::CENTER_CENTER, glyph, FontId::proportional(14.0), fade(color));
                        ui.painter().text(pos2(r.left() + 31.0, r.center().y), Align2::LEFT_CENTER, text, semi(13.0), fade(FG));
                        if *kind != 1 && ui.interact(*r, Id::new(("bubble", i)), Sense::click()).clicked() {
                            if *kind == 0 {
                                open_timer = true;
                            } else {
                                if *kind == 3 {
                                    open_dl = true;
                                } else {
                                    run_hint = true;
                                }
                            }
                        }
                    }
                    if open_dl {
                        sys::open(&format!("{}\\Downloads", std::env::var("USERPROFILE").unwrap_or_default()));
                    }
                    if run_hint {
                        if let Some(h) = self.hint.clone() {
                            self.do_hint(h.act);
                        }
                    }
                    if open_timer {
                        self.mode = Mode::Panel;
                        self.tab = Tab::Timer;
                    }
                }
                Mode::Panel => {
                    let inner = Rect::from_min_max(layout.min + vec2(22.0, 12.0), layout.max - vec2(22.0, 18.0));
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
                    child.set_clip_rect(rect.shrink(2.0));
                    child.set_opacity(mode_t);
                    self.ui_panel(&mut child, ctx);
                    let face = ui.interact(Rect::from_min_size(face_at, vec2(38.0, 18.0)).expand(4.0), Id::new("face"), Sense::click());
                    if face.clicked() {
                        self.next_emotion(now);
                    }
                    self.draw_face(&ui.painter().with_clip_rect(rect), face_at, look, now);
                }
                Mode::Palette => {
                    let inner = Rect::from_min_max(layout.min + vec2(16.0, 12.0), layout.max - vec2(12.0, 8.0));
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner));
                    child.set_clip_rect(rect.shrink(2.0));
                    child.set_opacity(mode_t);
                    self.ui_palette(&mut child);
                }
            }
        });

        // встроенный чат показывается, когда панель раскрылась и вкладка «ИИ» на месте
        let settled = !animating && mode_t >= 1.0 && self.mode == Mode::Panel && !invisible;
        let slot = if settled { self.web_slot } else { None };
        self.sync_web(frame, ctx, slot);

        // частота перерисовки: быстро только когда есть на что смотреть
        let playing = self.media_now.as_ref().is_some_and(|m| m.playing);
        if self.hidden && self.shot.is_none() && !animating {
            // остров спрятан (полноэкранная игра): кадры не нужны, только изредка проверяем, не пора ли показаться
            ctx.request_repaint_after(Duration::from_millis(400));
        } else if animating || self.shot.is_some() {
            ctx.request_repaint();
        } else if self.mode != Mode::Pill || dist < 420.0 || playing {
            ctx.request_repaint_after(Duration::from_millis(16));
        } else if self.precipitation() > 0 {
            // дождь и снег идут плавно, но без гонки кадров
            ctx.request_repaint_after(Duration::from_millis(33));
        } else {
            ctx.request_repaint_after(Duration::from_millis(90));
        }
    }
}

impl App {
    /// Корпус острова: толстое матовое стекло. Фон под ним размывает сама Windows; здесь рисуется
    /// объём — светлая кромка, радужный отблеск по ободку, мягкое свечение толщины и тень в глубине.
    fn draw_glass(&self, p: &egui::Painter, rect: Rect, r: f32, light: Vec2, _body: u8) {
        use egui::epaint::{Vertex, WHITE_UV};
        const N: usize = 14;
        fn strip(mesh: &mut egui::Mesh, rings: &[Vec<(Pos2, Color32)>]) {
            let base = mesh.vertices.len() as u32;
            let m = rings[0].len() as u32;
            for ring in rings {
                for (pos, color) in ring {
                    mesh.vertices.push(Vertex { pos: *pos, uv: WHITE_UV, color: *color });
                }
            }
            for li in 0..rings.len() as u32 - 1 {
                for i in 0..m {
                    let (a, b) = (base + li * m + i, base + li * m + (i + 1) % m);
                    mesh.indices.extend_from_slice(&[a, b, a + m, b, b + m, a + m]);
                }
            }
        }
        // тень под островом и цветная вспышка при событиях
        let round = Rounding::same(r);
        let shadow = egui::epaint::Shadow { offset: vec2(0.0, 6.0), blur: 30.0, spread: 0.0, color: rgba(0, 0, 0, 120) };
        p.add(shadow.as_shape(rect, round));
        if let (Some(c), true) = (self.album, self.media_now.as_ref().is_some_and(|m| m.playing)) {
            let breathe = 0.5 + 0.5 * (self.drip_phase * 0.7).sin();
            let aura = egui::epaint::Shadow { offset: vec2(0.0, 2.0), blur: 24.0 + 6.0 * breathe + 12.0 * self.beat, spread: 0.0, color: rgba(c[0], c[1], c[2], (70.0 + 50.0 * breathe + 70.0 * self.beat).min(255.0) as u8) };
            p.add(aura.as_shape(rect, round));
        }
        if let Some((color, at)) = self.glow {
            let t = at.elapsed().as_secs_f32() / 1.3;
            if t < 1.0 {
                let k = (1.0 - t).powi(3);
                let flash = egui::epaint::Shadow { offset: Vec2::ZERO, blur: 26.0 + 16.0 * k, spread: 2.0 * k, color: rgba(color.r(), color.g(), color.b(), (210.0 * k) as u8) };
                p.add(flash.as_shape(rect, round));
            }
        }

        // контур острова с «жидкой» деформацией: течение к курсору, рябь, капли под музыку
        let wp = self.warp;
        let pts_of = |inset: f32| -> Vec<(Pos2, Vec2)> {
            let c = contour(rect, r, inset.min(r - 0.5), N);
            let len = c.len() as f32;
            c.into_iter().enumerate().map(|(i, (pos, n))| (wp.apply(rect, pos, n, i as f32 / len), n)).collect()
        };
        let ring = |inset: f32, color: &dyn Fn(Pos2, Vec2) -> Color32| -> Vec<(Pos2, Color32)> { pts_of(inset).into_iter().map(|(pos, n)| (pos, color(pos, n))).collect() };
        let white = |a: f32| rgba(255, 255, 255, (a.clamp(0.0, 1.0) * 255.0) as u8);
        let black = |a: f32| rgba(0, 0, 0, (a.clamp(0.0, 1.0) * 255.0) as u8);
        let lens = self.lens_tex.as_ref().zip(self.lens_draw);
        let glass = self.acrylic == Some(true) || lens.is_some();
        if let Some((tex, cap)) = lens {
            let mut back = egui::Mesh::with_texture(tex.id());
            let uv = |q: Pos2| pos2(((q.x - cap.left()) / cap.width()).clamp(0.0, 1.0), ((q.y - cap.top()) / cap.height()).clamp(0.0, 1.0));
            let pts = pts_of(0.0);
            let count = pts.len() as u32;
            for (pos, _) in &pts {
                back.vertices.push(Vertex { pos: *pos, uv: uv(*pos), color: Color32::WHITE });
            }
            back.vertices.push(Vertex { pos: rect.center(), uv: uv(rect.center()), color: Color32::WHITE });
            for i in 0..count {
                back.indices.extend_from_slice(&[i, (i + 1) % count, count]);
            }
            p.add(egui::Shape::mesh(back));
        }

        // тело: без размытия — плотное тёмное, с размытием — лёгкая дымка, светлее сверху
        let body_at = |pos: Pos2| {
            let k = ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
            if glass {
                rgba((22.0 - 11.0 * k) as u8, (22.0 - 11.0 * k) as u8, (34.0 - 14.0 * k) as u8, if lens.is_some() { (118.0 + 30.0 * k) as u8 } else { 136 })
            } else {
                Color32::from_rgb((30.0 - 14.0 * k) as u8, (30.0 - 14.0 * k) as u8, (44.0 - 18.0 * k) as u8)
            }
        };
        let mut mesh = egui::Mesh::default();
        strip(&mut mesh, &[ring(-1.0, &|_, _| Color32::TRANSPARENT), ring(0.0, &|pos, _| body_at(pos))]);
        let base = mesh.vertices.len() as u32;
        let edge = pts_of(0.0);
        let m = edge.len() as u32;
        for (pos, _) in &edge {
            mesh.vertices.push(Vertex { pos: *pos, uv: WHITE_UV, color: body_at(*pos) });
        }
        mesh.vertices.push(Vertex { pos: rect.center(), uv: WHITE_UV, color: body_at(rect.center()) });
        for i in 0..m {
            mesh.indices.extend_from_slice(&[base + i, base + (i + 1) % m, base + m]);
        }

        if self.flask_on() {
            // жидкость: столбцы от поверхности до дна, обрезанные по форме капсулы
            let c = self.liquid_color();
            let (x0, x1) = (rect.left() + 2.5, rect.right() - 2.5);
            let cols = ((x1 - x0) / 3.0).ceil().max(2.0) as usize;
            let base = mesh.vertices.len() as u32;
            let mut surface = Vec::with_capacity(cols + 1);
            for k in 0..=cols {
                let x = x0 + (x1 - x0) * k as f32 / cols as f32;
                let (top, bottom, free) = self.surface_at(rect, r, x);
                let depth = ((bottom - top) / rect.height()).clamp(0.0, 1.0);
                mesh.vertices.push(Vertex { pos: pos2(x, top), uv: WHITE_UV, color: rgba(c[0], c[1], c[2], 150) });
                mesh.vertices.push(Vertex { pos: pos2(x, bottom), uv: WHITE_UV, color: rgba((c[0] as f32 * 0.55) as u8, (c[1] as f32 * 0.55) as u8, (c[2] as f32 * 0.6) as u8, (150.0 + 70.0 * depth) as u8) });
                if free {
                    surface.push(pos2(x, top));
                }
            }
            for k in 0..cols as u32 {
                let a = base + k * 2;
                mesh.indices.extend_from_slice(&[a, a + 2, a + 1, a + 2, a + 3, a + 1]);
            }
            // светлая линия по поверхности
            for w in surface.windows(2) {
                let (a, b) = (w[0], w[1]);
                let i = mesh.vertices.len() as u32;
                for (pos, alpha) in [(a - vec2(0.0, 0.6), 150u8), (b - vec2(0.0, 0.6), 150), (b + vec2(0.0, 1.1), 0), (a + vec2(0.0, 1.1), 0)] {
                    mesh.vertices.push(Vertex { pos, uv: WHITE_UV, color: rgba(255, 255, 255, alpha) });
                }
                mesh.indices.extend_from_slice(&[i, i + 1, i + 2, i, i + 2, i + 3]);
            }
        }

        // тень в глубине: стекло темнеет к нижней кромке и с теневой стороны
        let dark = |n: Vec2| 0.20 * n.y.max(0.0) + 0.16 * n.dot(-light).max(0.0);
        strip(&mut mesh, &[ring(1.5, &|_, n| black(dark(n))), ring(7.0, &|_, n| black(dark(n) * 0.45)), ring(18.0, &|_, _| Color32::TRANSPARENT)]);

        // толщина: мягкое светлое свечение от кромки внутрь
        strip(
            &mut mesh,
            &[ring(0.0, &|_, n| white(0.16 + 0.10 * (-n.y).max(0.0))), ring(5.0, &|_, n| white(0.05 + 0.05 * (-n.y).max(0.0))), ring(14.0, &|_, _| Color32::TRANSPARENT)],
        );

        // радужный отблеск по ободку: виден там, куда «светит» курсор, и напротив
        let iris = |pos: Pos2, n: Vec2, k: f32| {
            let amount = n.dot(light).max(0.0).powi(2) + 0.55 * n.dot(-light).max(0.0).powi(3);
            let angle = (pos - rect.center()).angle() / std::f32::consts::TAU;
            hue(angle * 2.0 + light.angle() / std::f32::consts::TAU, amount * k)
        };
        strip(
            &mut mesh,
            &[
                ring(0.4, &|p, n| iris(p, n, 0.0)),
                ring(1.6, &|p, n| iris(p, n, if lens.is_some() { 0.0 } else { 0.78 })),
                ring(3.4, &|p, n| iris(p, n, if lens.is_some() { 0.0 } else { 0.44 })),
                ring(6.0, &|_, _| Color32::TRANSPARENT),
            ],
        );

        // кромка: тонкая яркая линия, сверху и со стороны света заметнее
        let lit = |n: Vec2| 0.30 + 0.42 * (-n.y).max(0.0) + 0.45 * n.dot(light).max(0.0).powi(2) + 0.14 * n.y.max(0.0).powi(3);
        strip(&mut mesh, &[ring(0.0, &|_, n| white(lit(n))), ring(1.2, &|_, n| white(lit(n) * 0.75)), ring(2.4, &|_, _| Color32::TRANSPARENT)]);
        p.add(egui::Shape::mesh(mesh));
    }

    /// Жидкость рисуется только в сложенной пилюле и если её не выключили в настройках.
    fn flask_on(&self) -> bool {
        false
    }

    /// Капля упала в жидкость: at — место по ширине (0…1), amp — сила.
    fn splash(&mut self, at: f32, amp: f32) {
        let n = self.liq_v.len();
        let i = ((at.clamp(0.0, 1.0)) * (n as f32 - 1.0)) as usize;
        self.liq_v[i] -= amp;
        if i + 1 < n {
            self.liq_v[i + 1] -= amp * 0.5;
        }
        if i > 0 {
            self.liq_v[i - 1] -= amp * 0.5;
        }
    }

    /// Цвет жидкости говорит о состоянии: звонок, таймер, музыка, перегруженный процессор.
    fn liquid_color(&self) -> [u8; 3] {
        if self.call_since.is_some() {
            [0x6E, 0x7B, 0xFF]
        } else if self.timer_end.is_some() {
            [0xFF, 0xB3, 0x40]
        } else if let (Some(c), true) = (self.album, self.media_now.as_ref().is_some_and(|m| m.playing)) {
            c
        } else if self.stats.cpu > 0.85 {
            [0xFF, 0x5C, 0x5C]
        } else {
            [185, 164, 255]
        }
    }

    /// Для точки x внутри капсулы: верх жидкости, дно и признак «это свободная поверхность, а не стенка».
    fn surface_at(&self, rect: Rect, r: f32, x: f32) -> (f32, f32, bool) {
        let rin = (r - 2.5).max(1.0);
        let cy = rect.center().y;
        let (lc, rc) = (rect.left() + r, rect.right() - r);
        let dxc = if x < lc { lc - x } else if x > rc { x - rc } else { 0.0 };
        let half = (rin * rin - dxc * dxc).max(0.0).sqrt();
        let (top_b, bot_b) = (cy - half, cy + half);
        let n = self.liq_h.len();
        let wave = if n == 0 {
            0.0
        } else {
            let f = ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0) * (n as f32 - 1.0);
            let i = f as usize;
            self.liq_h[i] + (self.liq_h[(i + 1).min(n - 1)] - self.liq_h[i]) * (f - i as f32)
        };
        let ys = cy + rin - self.level * 2.0 * rin + wave;
        (ys.clamp(top_b, bot_b), bot_b, ys > top_b + 0.3 && ys < bot_b - 0.3)
    }

    fn weather_tips_h(&self) -> f32 {
        let has = self.weather.as_ref().is_some_and(|w| !weather::advice(w.feels, w.wind, &w.hours).is_empty());
        // строка фишек 26 px + отступ 6 px + запас, чтобы карточка Луны не упиралась в край
        if has { 44.0 } else { 0.0 }
    }

    fn panel_height(&self) -> f32 {
        // навигация теперь в одну строку
        let saved = if matches!(self.tab, Tab::Ai | Tab::Settings) { 0.0 } else { 24.0 };
        -saved + match self.tab {
            Tab::Home => 396.0,
            Tab::Control => 276.0,
            Tab::Sound => 322.0,
            Tab::Music => 388.0,
            // напоминания: страница растёт по их числу (до шести без прокрутки)
            Tab::Remind if self.cfg.reminders.is_empty() => 320.0,
            Tab::Remind => 200.0 + self.cfg.reminders.len().clamp(3, 6) as f32 * 42.0,
            Tab::Qr => 384.0,
            // полка растёт по рядам значков (в ряду помещается шесть)
            Tab::Shelf => (150.0 + ((self.cfg.shelf.len().max(1) + 5) / 6) as f32 * 118.0).clamp(270.0, 400.0),
            Tab::Ai if self.api_key().is_empty() => AI_H,
            Tab::Ai => 420.0,
            Tab::Jarvis => 462.0,
            // +74 — карточка Луны и солнца под прогнозом
            // +32 — строка подсказок «что надеть», когда есть что сказать
            Tab::Weather if self.weather.as_ref().is_some_and(|w| w.hours.len() >= 4) => 398.0 + 74.0 + self.weather_tips_h(),
            Tab::Weather if self.weather.is_some() => 292.0 + 74.0 + self.weather_tips_h(),
            Tab::Weather => 250.0,
            Tab::Timer => 348.0,
            // +52 — ряд мировых часов под календарём
            Tab::Calendar => 360.0 + 52.0,
            Tab::Shot => 306.0,
            Tab::Clip => 340.0 + if self.clip_imgs.is_empty() { 0.0 } else { 72.0 },
            Tab::Inbox => 340.0,
            // заметка с чек-листом: страница выше, чтобы пункты и редактор не теснили друг друга
            Tab::Notes => {
                let items = self.notes.get(self.note_sel).map_or(0, |n| store::check_items(n).len());
                340.0 + (items.min(5) as f32 * 34.0 + if items > 0 { 30.0 } else { 0.0 })
            }
            // дела: страница растёт по числу дел (до шести видно без прокрутки)
            Tab::Todo => 190.0 + self.cfg.todos.len().clamp(3, 6) as f32 * 44.0,
            Tab::System => if self.stats.battery.is_some() { 416.0 } else { 394.0 },
            Tab::Settings => 528.0,
        }
    }

    fn next_emotion(&mut self, now: Instant) {
        self.emotion = match self.emotion {
            Emotion::Normal => Emotion::Happy,
            Emotion::Happy => Emotion::Love,
            Emotion::Love => Emotion::Angry,
            Emotion::Angry => Emotion::Surprised,
            Emotion::Surprised => Emotion::Cool,
            Emotion::Cool => Emotion::Sleepy,
            Emotion::Sleepy => Emotion::Happy,
        };
        self.emotion_until = now + Duration::from_millis(2800);
    }
}

/// Экранная координата X начала «поля» острова, в точках.
fn base_x(ppp: f32) -> f32 {
    if let Some(x) = std::env::var("OSTROVOK_TEST_X").ok().and_then(|v| v.parse::<f32>().ok()) {
        x
    } else if test_mode() {
        -6000.0
    } else {
        ((sys::screen_width() as f32 / ppp - WIN_W) / 2.0).max(0.0)
    }
}

/// Системный идентификатор окна Островка.
fn hwnd_of(frame: &eframe::Frame) -> Option<isize> {
    use wry::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// Чистый цвет радуги: h от 0 до 1, a — непрозрачность.
/// Время суток: 0 — рассвет, 1 — день, 2 — закат, 3 — ночь. Рассвет и закат длятся по часу в обе стороны.
fn day_phase(h: f32, rise: f32, set: f32) -> u8 {
    if (h - rise).abs() < 1.0 {
        0
    } else if (h - set).abs() < 1.0 {
        2
    } else if h > rise && h < set {
        1
    } else {
        3
    }
}

/// Цвета неба (верх, низ) для фаза суток — полупрозрачные, чтобы цифры поверх читались.
fn sky_colors(phase: u8) -> (Color32, Color32) {
    let c = |r: u8, g: u8, b: u8, a: u8| Color32::from_rgba_unmultiplied(r, g, b, a);
    match phase {
        0 => (c(120, 110, 220, 40), c(255, 160, 120, 46)),
        1 => (c(90, 165, 255, 46), c(120, 200, 255, 8)),
        2 => (c(150, 90, 200, 40), c(255, 120, 110, 50)),
        _ => (c(70, 70, 170, 56), c(20, 20, 60, 10)),
    }
}

/// Скруглённый прямоугольник, залитый градиентом: цвет каждой точки контура задаёт функция.
fn gradient_rrect(p: &egui::Painter, rect: Rect, r: f32, col: impl Fn(Pos2) -> Color32) {
    let pts = contour(rect, r.min(rect.width() / 2.0).min(rect.height() / 2.0), 0.0, 8);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.center(), col(rect.center()));
    for (q, _) in &pts {
        mesh.colored_vertex(*q, col(*q));
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    p.add(egui::Shape::mesh(mesh));
}

/// Синодический месяц, дни.
const LUNAR: f64 = 29.530_588_9;

/// Возраст Луны в днях от новолуния (опорное новолуние — 6 января 2000, 18:14 UTC). Точность ±1 день.
fn moon_age(date: chrono::NaiveDate) -> f64 {
    let base = chrono::NaiveDate::from_ymd_opt(2000, 1, 6).unwrap().and_hms_opt(18, 14, 0).unwrap();
    let days = (date.and_hms_opt(12, 0, 0).unwrap() - base).num_minutes() as f64 / 1440.0;
    days.rem_euclid(LUNAR)
}

/// Освещённость 0..1, название фазы, растёт ли, дней до полнолуния.
fn moon_phase(age: f64) -> (f32, &'static str, bool, u32) {
    let illum = ((1.0 - (std::f64::consts::TAU * age / LUNAR).cos()) / 2.0) as f32;
    let name = match age {
        a if a < 1.0 || a >= LUNAR - 1.0 => "Новолуние",
        a if a < 6.4 => "Растущий серп",
        a if a < 8.4 => "Первая четверть",
        a if a < 13.8 => "Растущая луна",
        a if a < 15.8 => "Полнолуние",
        a if a < 21.1 => "Убывающая луна",
        a if a < 23.1 => "Последняя четверть",
        _ => "Убывающий серп",
    };
    let full = LUNAR / 2.0;
    // в пределах полусуток от полнолуния — «сегодня», а не «через 29 дней»
    let to_full = if (age - full).abs() < 0.5 {
        0.0
    } else if age <= full {
        full - age
    } else {
        LUNAR - age + full
    };
    (illum, name, age < full, to_full.round() as u32)
}

/// Долгота дня в минутах по восходу и закату в часах (7.5 = 07:30).
fn day_length(rise: f32, set: f32) -> u32 {
    ((set - rise).max(0.0) * 60.0).round() as u32
}

/// Луна: тёмный диск и освещённая часть — полукруг плюс терминатор-эллипс.
fn moon_disc(p: &egui::Painter, c: Pos2, r: f32, illum: f32, waxing: bool) {
    p.circle_filled(c, r + 6.0, rgba(230, 230, 255, 14));
    p.circle_filled(c, r, Color32::from_rgb(38, 38, 58));
    let light = Color32::from_rgb(240, 236, 214);
    if illum < 0.01 {
        p.circle_stroke(c, r, Stroke::new(1.0_f32, rgba(255, 255, 255, 40)));
        return;
    }
    // освещённый край: справа у растущей (северное полушарие), слева у убывающей
    let side = if waxing { 1.0 } else { -1.0 };
    let k = 1.0 - 2.0 * illum; // полуось терминатора: 1 — тонкий серп, -1 — полный диск
    let n = 40;
    let mut pts = Vec::with_capacity(2 * n + 2);
    for i in 0..=n {
        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::PI * i as f32 / n as f32;
        pts.push(c + vec2(side * a.cos() * r, a.sin() * r));
    }
    for i in 0..=n {
        let a = std::f32::consts::FRAC_PI_2 - std::f32::consts::PI * i as f32 / n as f32;
        pts.push(c + vec2(side * k * a.cos() * r, a.sin() * r));
    }
    // фигура может быть невыпуклой (серп) — рисуем веером из центра терминатора треугольниками
    let mut mesh = egui::Mesh::default();
    for q in &pts {
        mesh.colored_vertex(*q, light);
    }
    for i in 0..=n as u32 {
        let (a, b) = (i, 2 * n as u32 + 1 - i);
        if i < n as u32 {
            mesh.add_triangle(a, a + 1, b);
            mesh.add_triangle(a + 1, b - 1, b);
        }
    }
    p.add(egui::Shape::mesh(mesh));
}

/// Принять найденный геокодером город: название, координаты, отметка проверки.
fn apply_city(cfg: &mut store::Settings, query: &str, lat: f64, lon: f64, name: &str) {
    cfg.city = query.to_string();
    cfg.lat = Some(lat);
    cfg.lon = Some(lon);
    cfg.city_name = name.to_string();
    cfg.city_checked = true;
}

/// Лента дня: сегодняшние напоминания как доля суток (0..1) и текст, по порядку.
fn day_marks(list: &[store::Reminder], day_start: i64, day_len: i64) -> Vec<(f32, String)> {
    let mut v: Vec<&store::Reminder> = list.iter().filter(|r| r.at >= day_start && r.at < day_start + day_len).collect();
    v.sort_by_key(|r| r.at);
    v.into_iter().map(|r| ((r.at - day_start) as f32 / day_len as f32, r.text.clone())).collect()
}

/// «1 дело», «3 дела», «5 дел».
fn dela_word(n: usize) -> String {
    let w = match (n % 10, n % 100) {
        (_, 11..=14) => "дел",
        (1, _) => "дело",
        (2..=4, _) => "дела",
        _ => "дел",
    };
    format!("{n} {w}")
}

const QUIET_LEVEL: f32 = 0.2;
const VOL_PRESETS: [f32; 4] = [0.25, 0.5, 0.75, 1.0];

/// Тихий режим: (новая громкость, что запомнить). Есть запомненное — возвращаем его.
fn quiet_toggle(level: f32, saved: Option<f32>) -> (f32, Option<f32>) {
    match saved {
        Some(prev) => (prev, None),
        None => (level.min(QUIET_LEVEL), Some(level)),
    }
}

/// Какой из пресетов громкости сейчас стоит (с допуском в 1%).
fn preset_index(level: f32) -> Option<usize> {
    VOL_PRESETS.iter().position(|p| (p - level).abs() < 0.01)
}

const BATT_GREEN: [u8; 3] = [0x5E, 0xE0, 0x8A];
const BATT_ORANGE: [u8; 3] = [0xFF, 0xB3, 0x40];
const BATT_RED: [u8; 3] = [0xFF, 0x5C, 0x5C];

/// Батарейка в пилюле: только когда заряда осталось 20% и меньше.
fn battery_seg(b: Option<(u8, bool)>) -> Option<(u8, bool)> {
    b.filter(|(pct, _)| *pct <= 20)
}

fn battery_rgb(pct: u8, charging: bool) -> [u8; 3] {
    if charging { BATT_GREEN } else if pct <= 10 { BATT_RED } else { BATT_ORANGE }
}

/// Положить путь на полку первым: без дублей, не больше 30. Возвращает вытесненные пути.
fn shelf_add(list: &mut Vec<String>, path: String) -> Vec<String> {
    list.retain(|p| p != &path);
    list.insert(0, path);
    if list.len() > 30 { list.split_off(30) } else { Vec::new() }
}

/// QR-код текста; None, если не помещается.
fn qr_encode(text: &str) -> Option<qrcode::QrCode> {
    qrcode::QrCode::new(text.as_bytes()).ok()
}

/// Кэш QR: текст и (ширина, тёмные клетки) либо None, если не помещается.
type QrCache = (String, Option<(usize, Vec<bool>)>);

/// QR из кэша; пересчитывает только при смене текста.
fn qr_cached<'a>(cache: &'a mut Option<QrCache>, text: &str) -> Option<&'a (usize, Vec<bool>)> {
    if cache.as_ref().is_none_or(|(t, _)| t != text) {
        let v = qr_encode(text).map(|c| (c.width(), c.to_colors().into_iter().map(|c| c == qrcode::Color::Dark).collect()));
        *cache = Some((text.to_string(), v));
    }
    cache.as_ref().and_then(|(_, v)| v.as_ref())
}

/// Мьютекс без паники: отравленный (поток упал под замком) всё равно читаем.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Предел числа значков в памяти.
const ICON_LIMIT: usize = 512;

/// Какие ключи значков выбросить: если их больше limit — все вне keep.
fn icon_keys_to_drop<'a>(keys: impl Iterator<Item = &'a str>, keep: &std::collections::HashSet<&str>, limit: usize) -> Vec<String> {
    let keys: Vec<&str> = keys.collect();
    if keys.len() <= limit {
        return Vec::new();
    }
    keys.into_iter().filter(|k| !keep.contains(k)).map(str::to_string).collect()
}

/// Подпись вида содержимого кода.
fn qr_kind(text: &str) -> &'static str {
    let has = |pre: &str| text.get(..pre.len()).is_some_and(|h| h.eq_ignore_ascii_case(pre));
    if has("http://") || has("https://") {
        "Ссылка"
    } else if text.starts_with("WIFI:") {
        "Wi‑Fi"
    } else {
        "Текст"
    }
}

#[cfg(test)]
mod fix_tests {
    use super::*;
    fn d(y: i32, m: u32, dd: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, m, dd).unwrap()
    }

    #[test]
    fn shelf_add_moves_to_front_without_dupes() {
        let mut l: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let out = shelf_add(&mut l, "b".into());
        assert_eq!(l, vec!["b", "a", "c"]);
        assert!(out.is_empty());
        shelf_add(&mut l, "z".into());
        assert_eq!(l, vec!["z", "b", "a", "c"]);
    }

    #[test]
    fn shelf_add_limits_to_30_and_returns_evicted() {
        let mut l: Vec<String> = (0..30).map(|i| i.to_string()).collect();
        let out = shelf_add(&mut l, "new".into());
        assert_eq!(l.len(), 30);
        assert_eq!(l[0], "new");
        assert_eq!(out, vec!["29".to_string()]);
        let out = shelf_add(&mut l, "5".into());
        assert!(out.is_empty());
        assert_eq!(l.len(), 30);
        assert_eq!(l.iter().filter(|p| *p == "5").count(), 1);
    }

    #[test]
    fn qr_long_cyrillic_errors_without_panic() {
        let s: String = std::iter::repeat('я').take(3000).collect();
        assert!(qr_encode(&s).is_none());
    }

    #[test]
    fn qr_emoji_encodes() {
        assert!(qr_encode("😀🎉🚀").is_some());
    }

    #[test]
    fn qr_kind_requires_scheme() {
        assert_eq!(qr_kind("https://a.ru"), "Ссылка");
        assert_eq!(qr_kind("http://a.ru"), "Ссылка");
        assert_eq!(qr_kind("httpfoo"), "Текст");
        assert_eq!(qr_kind("WIFI:S:x;;"), "Wi‑Fi");
    }

    #[test]
    fn parse_due_keeps_newlines() {
        let (t, dt) = parse_due("купить\nхлеб до 5.12", d(2026, 11, 17));
        assert_eq!(t, "купить\nхлеб");
        assert_eq!(dt, Some(d(2026, 12, 5)));
    }

    #[test]
    fn parse_due_emoji_tomorrow() {
        assert_eq!(parse_due("😀 завтра", d(2026, 11, 17)), ("😀".to_string(), Some(d(2026, 11, 18))));
    }

    #[test]
    fn parse_due_empty_and_bare() {
        let t = d(2026, 11, 17);
        assert_eq!(parse_due("", t).1, None);
        assert_eq!(parse_due("   ", t).1, None);
        assert_eq!(parse_due("до пятницы", t), ("до пятницы".to_string(), None));
    }
}

/// Срок в конце текста дела: «сегодня», «завтра», «послезавтра», «до пятницы», «до 5.12».
/// Возвращает текст без срока и дату; если срока нет — текст как есть.
fn parse_due(text: &str, today: chrono::NaiveDate) -> (String, Option<chrono::NaiveDate>) {
    use chrono::Datelike;
    let plain = (text.trim().to_string(), None);
    // слова с байтовыми позициями — хвост отрезаем по позиции, не трогая пробелы и переводы строк
    let base = text.as_ptr() as usize;
    let starts: Vec<usize> = text.split_whitespace().map(|w| w.as_ptr() as usize - base).collect();
    let words: Vec<&str> = text.split_whitespace().collect();
    let Some(last) = words.last() else { return plain };
    let low = last.to_lowercase();
    let low = low.trim_end_matches(['.', '!', ',']);
    let date = match low {
        "сегодня" => Some(today),
        "завтра" => today.succ_opt(),
        "послезавтра" => today.succ_opt().and_then(|d| d.succ_opt()),
        _ => {
            const WD: [&str; 7] = ["понедельника", "вторника", "среды", "четверга", "пятницы", "субботы", "воскресенья"];
            if let Some(k) = WD.iter().position(|w| *w == low) {
                let now = today.weekday().num_days_from_monday() as i64;
                let ahead = (k as i64 - now).rem_euclid(7);
                Some(today + chrono::Duration::days(if ahead == 0 { 7 } else { ahead }))
            } else if let Some((dd, mm)) = low.split_once('.') {
                match (dd.parse::<u32>(), mm.parse::<u32>()) {
                    // ближайшая такая дата не раньше сегодня; 29.02 — до следующего високосного года
                    (Ok(dd), Ok(mm)) => (today.year()..=today.year() + 8).filter_map(|y| chrono::NaiveDate::from_ymd_opt(y, mm, dd)).find(|d| *d >= today),
                    _ => None,
                }
            } else {
                None
            }
        }
    };
    let Some(date) = date else { return plain };
    // числа и дни недели — только после «до»; «сегодня/завтра» можно и без него
    let needs_do = !matches!(low, "сегодня" | "завтра" | "послезавтра");
    let mut keep = words.len() - 1;
    if keep > 0 && words[keep - 1].to_lowercase() == "до" {
        keep -= 1;
    } else if needs_do {
        return plain;
    }
    if keep == 0 {
        return plain;
    }
    (text[..starts[keep]].trim().to_string(), Some(date))
}

/// Подпись срока и тревога: 0 — спокойно, 1 — сегодня, 2 — просрочено.
fn due_label(due: chrono::NaiveDate, today: chrono::NaiveDate) -> (String, u8) {
    let days = (due - today).num_days();
    match days {
        0 => ("сегодня".into(), 1),
        d if d < 0 => (format!("просрочено {} д", -d), 2),
        1 => ("завтра".into(), 0),
        _ => due_date_label(due),
    }
}

/// «пт, 20 нояб» — дата срока без оценки.
fn due_date_label(due: chrono::NaiveDate) -> (String, u8) {
    use chrono::Datelike;
    const WD: [&str; 7] = ["пн", "вт", "ср", "чт", "пт", "сб", "вс"];
    const MON: [&str; 12] = ["янв", "фев", "мар", "апр", "мая", "июн", "июл", "авг", "сен", "окт", "нояб", "дек"];
    (format!("{}, {} {}", WD[due.weekday().num_days_from_monday() as usize], due.day(), MON[due.month0() as usize]), 0)
}

/// Новое дело из строки: срок вынимается из текста.
fn new_todo(text: &str) -> store::Todo {
    let (text, due) = parse_due(text, chrono::Local::now().date_naive());
    store::Todo { text, done: false, due: due.map(|d| d.format("%Y-%m-%d").to_string()) }
}

/// «Сон через 29:41» — подпись отложенного действия питания.
fn power_timer_label(action: &str, secs_left: u64) -> String {
    let name = if action == "sleep" { "Сон" } else { "Блокировка" };
    format!("{name} через {}:{:02}", secs_left / 60, secs_left % 60)
}

/// Отложенное действие: (пора выполнить, пора предупредить за минуту).
fn power_due(secs_left: u64, warned: bool) -> (bool, bool) {
    (secs_left == 0, !warned && secs_left > 0 && secs_left <= 60)
}

/// Какую тему показывать в предпросмотре: под курсором, иначе выбранную.
fn theme_preview_idx(hovered: Option<usize>, current: usize) -> usize {
    hovered.unwrap_or(current).min(THEMES.len() - 1)
}

/// Оттенок стекла темы, осветлённый для кружков и предпросмотра.
fn glass_shade(g: [u8; 3]) -> [u8; 3] {
    g.map(|c| c.saturating_mul(3))
}

/// Ширина мини-пилюли: глаза слева (28), название, поле справа (12).
fn theme_preview_w(name_w: f32) -> f32 {
    28.0 + name_w + 12.0
}

/// Официальные праздничные дни России (ст. 112 ТК РФ). Переносы выходных не учитываются.
fn ru_holiday(month: u32, day: u32) -> Option<&'static str> {
    Some(match (month, day) {
        (1, 1) => "Новый год",
        (1, 7) => "Рождество Христово",
        (1, 2..=8) => "Новогодние каникулы",
        (2, 23) => "День защитника Отечества",
        (3, 8) => "Международный женский день",
        (5, 1) => "Праздник Весны и Труда",
        (5, 9) => "День Победы",
        (6, 12) => "День России",
        (11, 4) => "День народного единства",
        _ => return None,
    })
}

/// Повтор напоминания: 0 — нет, 1 — каждый день, 2 — по будням, 3 — каждую неделю.
fn repeat_name(repeat: u8) -> &'static str {
    match repeat {
        1 => "каждый день",
        2 => "по будням",
        3 => "каждую неделю",
        _ => "без повтора",
    }
}

/// Следующий срок повторяющегося напоминания — первый позже `now`, по местному времени.
fn next_due(at: chrono::NaiveDateTime, repeat: u8, now: chrono::NaiveDateTime) -> Option<chrono::NaiveDateTime> {
    use chrono::Datelike;
    let step = match repeat {
        1 | 2 => chrono::Duration::days(1),
        3 => chrono::Duration::days(7),
        _ => return None,
    };
    let mut next = at;
    // ограничение на всякий случай: больше чем за десять лет не проматываем
    for _ in 0..4000 {
        next += step;
        if repeat == 2 && next.weekday().num_days_from_monday() >= 5 {
            continue;
        }
        if next > now {
            return Some(next);
        }
    }
    None
}

/// Новый срок сработавшего повторяющегося напоминания — строго позже `stamp`.
/// В двусмысленный осенний час «первый из двух» мог дать момент раньше текущего,
/// и напоминание удалялось навсегда; теперь проматываем дальше.
fn reschedule<T: chrono::TimeZone>(at: chrono::NaiveDateTime, repeat: u8, now: chrono::NaiveDateTime, stamp: i64, lookup: impl Fn(chrono::NaiveDateTime) -> chrono::LocalResult<chrono::DateTime<T>>) -> Option<i64> {
    let mut next = next_due(at, repeat, now)?;
    for _ in 0..8 {
        // в двусмысленный час подходит любой из двух моментов, лишь бы ещё впереди
        let cands: Vec<i64> = match lookup(next) {
            chrono::LocalResult::Ambiguous(x, y) => vec![x.timestamp(), y.timestamp()],
            _ => resolve_local(next, &lookup).map(|d| d.timestamp()).into_iter().collect(),
        };
        if let Some(ts) = cands.into_iter().find(|ts| *ts > stamp) {
            return Some(ts);
        }
        next = next_due(next, repeat, next)?;
    }
    None
}

/// Что показывать на циферблате: (секунд осталось, доля, идёт ли, на паузе ли).
fn timer_view(end: Option<Instant>, paused: Option<Duration>, total: f32, now: Instant) -> (u64, f32, bool, bool) {
    let total = total.max(1.0);
    match (end, paused) {
        (Some(e), _) => {
            let left = e.saturating_duration_since(now).as_secs_f32();
            (left.ceil() as u64, left / total, true, false)
        }
        (None, Some(left)) => (left.as_secs_f32().ceil() as u64, left.as_secs_f32() / total, false, true),
        _ => (0, 0.0, false, false),
    }
}

/// Номера быстрых действий, которые не скрыты, в исходном порядке.
fn visible_actions(ids: &[&str], hidden: &[String]) -> Vec<usize> {
    (0..ids.len()).filter(|i| !hidden.iter().any(|h| h == ids[*i])).collect()
}

/// Скрыть действие: известное, ещё не скрытое и не последнее видимое.
fn hide_action(ids: &[&str], hidden: &[String], id: &str) -> Vec<String> {
    let mut out = hidden.to_vec();
    if ids.contains(&id) && !out.iter().any(|h| h == id) && visible_actions(ids, hidden).len() > 1 {
        out.push(id.to_string());
    }
    out
}

/// Экранирование для строки Wi-Fi в QR: обратная косая, ; , : и кавычка.
fn wifi_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '\\' | ';' | ',' | ':' | '"') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Строка для QR-кода Wi-Fi; пустая, если сеть не указана.
fn wifi_payload(ssid: &str, pass: &str, secure: bool) -> String {
    if ssid.trim().is_empty() {
        return String::new();
    }
    if secure {
        format!("WIFI:T:WPA;S:{};P:{};;", wifi_escape(ssid), wifi_escape(pass))
    } else {
        format!("WIFI:T:nopass;S:{};;", wifi_escape(ssid))
    }
}

/// Допустимое время таймера: больше нуля и не больше суток (NaN — нет: «nan» парсится как число).
/// Местное время → метка. В ночь перевода часов 02:30 бывает дважды (берём первое)
/// или не бывает вовсе (весной в Германии 02:00→03:00 — сдвигаем на час вперёд).
#[cfg(test)]
fn local_ts(t: chrono::NaiveDateTime) -> Option<i64> {
    use chrono::TimeZone;
    let tz = chrono::Local;
    tz.from_local_datetime(&t).earliest().or_else(|| tz.from_local_datetime(&(t + chrono::Duration::hours(1))).earliest()).map(|d| d.timestamp())
}

fn timer_minutes_ok(m: f64) -> bool {
    m.is_finite() && m > 0.0 && m <= 24.0 * 60.0
}

/// Демо отложенного сна: секунд до срока, без ухода в минус при 0 минут.
fn power_demo_secs(m: u64) -> u64 {
    m.saturating_mul(60).saturating_sub(19)
}

/// Местное время → момент с учётом перевода часов: в двусмысленный час — первый из двух,
/// в несуществующий (весной) — на час позже. `lookup` — пояс (обычно `Local`), чтобы тестировать без машины.
fn resolve_local<T>(n: chrono::NaiveDateTime, lookup: impl Fn(chrono::NaiveDateTime) -> chrono::LocalResult<T>) -> Option<T> {
    match lookup(n) {
        chrono::LocalResult::Single(d) => Some(d),
        chrono::LocalResult::Ambiguous(a, _) => Some(a),
        chrono::LocalResult::None => lookup(n + chrono::Duration::hours(1)).earliest(),
    }
}

/// То же для пояса машины.
fn resolve_here(n: chrono::NaiveDateTime) -> Option<chrono::DateTime<chrono::Local>> {
    use chrono::TimeZone;
    resolve_local(n, |x| chrono::Local.from_local_datetime(&x))
}

#[cfg(test)]
mod dst_tests {
    use super::*;
    use chrono::{FixedOffset, LocalResult, NaiveDateTime, TimeZone};
    fn t(y: i32, m: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, mi, 0).unwrap()
    }
    /// Берлин 2026 без привязки к поясу машины: 29.03 02:00–03:00 нет, 25.10 02:00–03:00 дважды.
    fn berlin(n: NaiveDateTime) -> LocalResult<chrono::DateTime<FixedOffset>> {
        let (cet, cest) = (FixedOffset::east_opt(3600).unwrap(), FixedOffset::east_opt(7200).unwrap());
        let (gap0, gap1) = (t(2026, 3, 29, 2, 0), t(2026, 3, 29, 3, 0));
        let (amb0, amb1) = (t(2026, 10, 25, 2, 0), t(2026, 10, 25, 3, 0));
        if n >= gap0 && n < gap1 {
            LocalResult::None
        } else if n >= amb0 && n < amb1 {
            LocalResult::Ambiguous(cest.from_local_datetime(&n).unwrap(), cet.from_local_datetime(&n).unwrap())
        } else if n >= gap1 && n < amb0 {
            cest.from_local_datetime(&n)
        } else {
            cet.from_local_datetime(&n)
        }
    }
    #[test]
    fn gap_moves_hour_forward() {
        let d = resolve_local(t(2026, 3, 29, 2, 30), berlin).unwrap();
        assert_eq!(d.naive_local(), t(2026, 3, 29, 3, 30));
    }
    #[test]
    fn ambiguous_takes_earliest() {
        let d = resolve_local(t(2026, 10, 25, 2, 30), berlin).unwrap();
        assert_eq!(d.offset().local_minus_utc(), 7200);
        assert_eq!(resolve_local(t(2026, 11, 1, 9, 0), berlin).unwrap().naive_local(), t(2026, 11, 1, 9, 0));
    }
    #[test]
    fn repeat_fired_in_ambiguous_hour_is_not_lost() {
        // ежедневное 02:45, ПК спал; проснулся 25.10 в 02:40 по зимнему (второй проход часа)
        let stamp = cet_ts(t(2026, 10, 25, 2, 40));
        let at = t(2026, 10, 24, 2, 45);
        let next = reschedule(at, 1, t(2026, 10, 25, 2, 40), stamp, berlin).unwrap();
        assert!(next > stamp, "новый срок должен быть в будущем, иначе напоминание удалится");
        assert_eq!(next, cet_ts(t(2026, 10, 25, 2, 45)), "второй проход часа ещё впереди");
        // обычный случай: следующий день, без лишних шагов
        assert_eq!(reschedule(t(2026, 11, 2, 9, 0), 1, t(2026, 11, 2, 9, 1), cet_ts(t(2026, 11, 2, 9, 1)), berlin), Some(cet_ts(t(2026, 11, 3, 9, 0))));
        assert_eq!(reschedule(t(2026, 11, 2, 9, 0), 0, t(2026, 11, 2, 9, 1), 0, berlin), None);
    }
    fn cet_ts(n: NaiveDateTime) -> i64 {
        FixedOffset::east_opt(3600).unwrap().from_local_datetime(&n).unwrap().timestamp()
    }
    #[test]
    fn daily_repeat_through_dst_days_survives() {
        // напоминание каждый день в 02:30: ни 29.03, ни 25.10 не теряются
        let next = next_due(t(2026, 3, 28, 2, 30), 1, t(2026, 3, 28, 2, 31)).unwrap();
        let at = resolve_local(next, berlin).unwrap();
        assert!(at.timestamp() > berlin(t(2026, 3, 28, 2, 31)).single().unwrap().timestamp());
        let next = next_due(t(2026, 10, 24, 2, 30), 1, t(2026, 10, 24, 2, 31)).unwrap();
        assert!(resolve_local(next, berlin).is_some());
    }
}

#[cfg(test)]
mod review_fix_tests {
    use super::*;
    #[test]
    fn demo_power_timer_never_underflows() {
        assert_eq!(power_demo_secs(0), 0);
        assert_eq!(power_demo_secs(30), 30 * 60 - 19);
    }
    #[test]
    fn timer_minutes_validation() {
        assert!(!timer_minutes_ok(0.0));
        assert!(!timer_minutes_ok(-3.0));
        assert!(!timer_minutes_ok(f64::NAN));
        assert!(!timer_minutes_ok(24.0 * 60.0 + 1.0));
        assert!(timer_minutes_ok(0.5));
        assert!(timer_minutes_ok(24.0 * 60.0));
    }
}

#[cfg(test)]
mod wifi_qr_tests {
    use super::*;
    #[test]
    fn escapes_special() {
        assert_eq!(wifi_escape("HomeNet"), "HomeNet");
        assert_eq!(wifi_escape(r#"a\b;c,d:e"f"#), r#"a\\b\;c\,d\:e\"f"#);
        assert_eq!(wifi_escape("Дача 5G"), "Дача 5G");
    }
    #[test]
    fn payload_format() {
        assert_eq!(wifi_payload("HomeNet", "example123", true), "WIFI:T:WPA;S:HomeNet;P:example123;;");
        assert_eq!(wifi_payload("Cafe;Free", "", false), "WIFI:T:nopass;S:Cafe\\;Free;;");
        // без пароля пароль не попадает в код, даже если вписан
        assert_eq!(wifi_payload("Open", "secret", false), "WIFI:T:nopass;S:Open;;");
        assert_eq!(wifi_payload("  ", "x", true), "");
    }
}

#[cfg(test)]
mod actions_row_tests {
    use super::*;
    const IDS: [&str; 4] = ["region", "full", "ocr", "qr"];
    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }
    #[test]
    fn visible_keeps_order() {
        assert_eq!(visible_actions(&IDS, &[]), vec![0, 1, 2, 3]);
        assert_eq!(visible_actions(&IDS, &s(&["full", "qr", "чужое"])), vec![0, 2]);
    }
    #[test]
    fn hide_never_empties() {
        assert_eq!(hide_action(&IDS, &[], "ocr"), s(&["ocr"]));
        // повторное скрытие ничего не меняет
        assert_eq!(hide_action(&IDS, &s(&["ocr"]), "ocr"), s(&["ocr"]));
        // последнее видимое не скрываем
        assert_eq!(hide_action(&IDS, &s(&["region", "full", "ocr"]), "qr"), s(&["region", "full", "ocr"]));
        // неизвестное не добавляем
        assert_eq!(hide_action(&IDS, &[], "zzz"), Vec::<String>::new());
    }
}

#[cfg(test)]
mod timer_pause_tests {
    use super::*;
    #[test]
    fn view_running_paused_idle() {
        let now = Instant::now();
        // идёт: осталось 30 из 60 секунд
        let (s, f, live, paused) = timer_view(Some(now + Duration::from_secs(30)), None, 60.0, now);
        assert_eq!((s, live, paused), (30, true, false));
        assert!((f - 0.5).abs() < 0.01);
        // пауза: остаток 15 из 60
        let (s, f, live, paused) = timer_view(None, Some(Duration::from_secs(15)), 60.0, now);
        assert_eq!((s, live, paused), (15, false, true));
        assert!((f - 0.25).abs() < 0.01);
        // ничего
        assert_eq!(timer_view(None, None, 60.0, now), (0, 0.0, false, false));
    }
    #[test]
    fn formats() {
        assert_eq!(fmt_secs(0), "00:00");
        assert_eq!(fmt_secs(125), "02:05");
        assert_eq!(fmt_secs(3725), "1:02:05");
    }
}

#[cfg(test)]
mod repeat_tests {
    use super::*;
    fn t(y: i32, m: u32, d: u32, h: u32) -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, 0, 0).unwrap()
    }
    #[test]
    fn next_terms() {
        // 20 ноября 2026 — пятница
        assert_eq!(next_due(t(2026, 11, 20, 9), 0, t(2026, 11, 20, 9)), None);
        assert_eq!(next_due(t(2026, 11, 20, 9), 1, t(2026, 11, 20, 9)), Some(t(2026, 11, 21, 9)));
        assert_eq!(next_due(t(2026, 11, 20, 9), 2, t(2026, 11, 20, 9)), Some(t(2026, 11, 23, 9)));
        assert_eq!(next_due(t(2026, 11, 20, 9), 3, t(2026, 11, 20, 9)), Some(t(2026, 11, 27, 9)));
        // конец месяца и года
        assert_eq!(next_due(t(2026, 1, 31, 8), 1, t(2026, 1, 31, 8)), Some(t(2026, 2, 1, 8)));
        assert_eq!(next_due(t(2026, 12, 28, 8), 3, t(2026, 12, 28, 8)), Some(t(2027, 1, 4, 8)));
        // будни: суббота сама по себе переходит на понедельник
        assert_eq!(next_due(t(2026, 11, 21, 9), 2, t(2026, 11, 21, 9)), Some(t(2026, 11, 23, 9)));
    }
    #[test]
    fn skips_missed() {
        // компьютер был выключен три дня — следующий срок первый после «сейчас»
        assert_eq!(next_due(t(2026, 11, 10, 9), 1, t(2026, 11, 13, 12)), Some(t(2026, 11, 14, 9)));
        assert_eq!(next_due(t(2026, 11, 2, 9), 3, t(2026, 11, 20, 10)), Some(t(2026, 11, 23, 9)));
        assert_eq!(repeat_name(2), "по будням");
        assert_eq!(repeat_name(9), "без повтора");
    }
}

#[cfg(test)]
mod holiday_tests {
    use super::*;
    #[test]
    fn russian_holidays() {
        assert_eq!(ru_holiday(1, 1), Some("Новый год"));
        assert_eq!(ru_holiday(1, 5), Some("Новогодние каникулы"));
        assert_eq!(ru_holiday(1, 7), Some("Рождество Христово"));
        assert_eq!(ru_holiday(1, 9), None);
        assert_eq!(ru_holiday(2, 23), Some("День защитника Отечества"));
        assert_eq!(ru_holiday(3, 8), Some("Международный женский день"));
        assert_eq!(ru_holiday(5, 1), Some("Праздник Весны и Труда"));
        assert_eq!(ru_holiday(5, 9), Some("День Победы"));
        assert_eq!(ru_holiday(6, 12), Some("День России"));
        assert_eq!(ru_holiday(11, 4), Some("День народного единства"));
        assert_eq!(ru_holiday(11, 17), None);
        assert_eq!(ru_holiday(13, 1), None);
    }
}

#[cfg(test)]
mod theme_preview_tests {
    use super::*;
    #[test]
    fn width_fits_column() {
        assert_eq!(theme_preview_w(60.0), 100.0);
        // шесть кружков по 30 + отступы + пилюля для самого длинного названия (~60 px) — меньше половины панели
        assert!(6.0 * 30.0 + 5.0 * 8.0 + 6.0 + theme_preview_w(60.0) < PANEL_W / 2.0 - 20.0);
    }
    #[test]
    fn picks_hovered_or_current() {
        assert_eq!(theme_preview_idx(Some(3), 0), 3);
        assert_eq!(theme_preview_idx(None, 2), 2);
        assert_eq!(theme_preview_idx(Some(99), 1), THEMES.len() - 1);
        assert_eq!(theme_preview_idx(None, 99), THEMES.len() - 1);
    }
    #[test]
    fn glass_is_lighter_and_safe() {
        assert_eq!(glass_shade([11, 11, 20]), [33, 33, 60]);
        assert_eq!(glass_shade([200, 90, 0]), [255, 255, 0]);
    }
}

#[cfg(test)]
mod power_timer_tests {
    use super::*;
    #[test]
    fn labels() {
        assert_eq!(power_timer_label("sleep", 1781), "Сон через 29:41");
        assert_eq!(power_timer_label("lock", 59), "Блокировка через 0:59");
        assert_eq!(power_timer_label("lock", 3600), "Блокировка через 60:00");
        assert_eq!(power_timer_label("sleep", 0), "Сон через 0:00");
    }
    #[test]
    fn due_and_warning() {
        // (сработать, предупредить)
        assert_eq!(power_due(120, false), (false, false));
        assert_eq!(power_due(60, false), (false, true));
        assert_eq!(power_due(30, true), (false, false));
        assert_eq!(power_due(0, true), (true, false));
    }
}

#[cfg(test)]
mod due_tests {
    use super::*;
    fn d(m: u32, day: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }
    // 17 ноября 2026 — вторник
    #[test]
    fn parse_words() {
        let t = d(11, 17);
        assert_eq!(parse_due("Обновить драйверы сегодня", t), ("Обновить драйверы".into(), Some(t)));
        assert_eq!(parse_due("Бэкап завтра", t), ("Бэкап".into(), Some(d(11, 18))));
        assert_eq!(parse_due("Бэкап до послезавтра", t), ("Бэкап".into(), Some(d(11, 19))));
        assert_eq!(parse_due("Купить SSD до пятницы", t), ("Купить SSD".into(), Some(d(11, 20))));
        assert_eq!(parse_due("Чистка до вторника", t), ("Чистка".into(), Some(d(11, 24))));
        assert_eq!(parse_due("Сдать отчёт до 5.12", t), ("Сдать отчёт".into(), Some(d(12, 5))));
        assert_eq!(parse_due("Продлить домен до 01.03", t), ("Продлить домен".into(), Some(chrono::NaiveDate::from_ymd_opt(2027, 3, 1).unwrap())));
    }
    #[test]
    fn parse_leaves_plain_text() {
        let t = d(11, 17);
        assert_eq!(parse_due("Переустановить Windows", t), ("Переустановить Windows".into(), None));
        assert_eq!(parse_due("завтра", t), ("завтра".into(), None));
        assert_eq!(parse_due("Разобраться до 31.02", t), ("Разобраться до 31.02".into(), None));
    }
    #[test]
    fn labels() {
        let t = d(11, 17);
        assert_eq!(due_label(t, t), ("сегодня".into(), 1));
        assert_eq!(due_label(d(11, 18), t), ("завтра".into(), 0));
        assert_eq!(due_label(d(11, 15), t), ("просрочено 2 д".into(), 2));
        assert_eq!(due_label(d(11, 20), t), ("пт, 20 нояб".into(), 0));
    }
}

#[cfg(test)]
mod battery_tests {
    use super::*;
    #[test]
    fn shows_only_when_low() {
        assert_eq!(battery_seg(None), None);
        assert_eq!(battery_seg(Some((80, false))), None);
        assert_eq!(battery_seg(Some((21, false))), None);
        assert_eq!(battery_seg(Some((20, false))), Some((20, false)));
        assert_eq!(battery_seg(Some((5, true))), Some((5, true)));
        assert_eq!(battery_seg(Some((60, true))), None);
    }
    #[test]
    fn colors() {
        assert_eq!(battery_rgb(15, true), BATT_GREEN);
        assert_eq!(battery_rgb(15, false), BATT_ORANGE);
        assert_eq!(battery_rgb(10, false), BATT_RED);
        assert_eq!(battery_rgb(0, false), BATT_RED);
    }
}

#[cfg(test)]
mod quiet_tests {
    use super::*;
    #[test]
    fn toggle_remembers_and_restores() {
        assert_eq!(quiet_toggle(0.7, None), (QUIET_LEVEL, Some(0.7)));
        assert_eq!(quiet_toggle(QUIET_LEVEL, Some(0.7)), (0.7, None));
        // громкость уже тише тихого — не поднимаем, но запоминаем
        assert_eq!(quiet_toggle(0.1, None), (0.1, Some(0.1)));
        // пока тихо, громкость сдвинули рукой — выход всё равно возвращает прежнюю
        assert_eq!(quiet_toggle(0.3, Some(0.9)), (0.9, None));
    }
    #[test]
    fn preset_match() {
        assert_eq!(preset_index(0.5), Some(1));
        assert_eq!(preset_index(0.995), Some(3));
        assert_eq!(preset_index(0.252), Some(0));
        assert_eq!(preset_index(0.4), None);
    }
}

#[cfg(test)]
mod day_strip_tests {
    use super::*;
    fn r(text: &str, at: i64) -> store::Reminder {
        store::Reminder { text: text.into(), at, repeat: 0 }
    }
    #[test]
    fn marks_only_today_sorted() {
        let day = 1_000_000;
        let list = [r("завтра", day + 86_400), r("вечер", day + 64_800), r("вчера", day - 1), r("утро", day + 21_600), r("полночь", day)];
        let m = day_marks(&list, day, 86_400);
        assert_eq!(m.len(), 3);
        assert_eq!(m[0], (0.0, "полночь".to_string()));
        assert_eq!(m[1], (0.25, "утро".to_string()));
        assert_eq!(m[2], (0.75, "вечер".to_string()));
        assert!(day_marks(&[], day, 86_400).is_empty());
    }
    #[test]
    fn marks_on_25_hour_day() {
        let day = 1_000_000;
        let len = 90_000;
        let list = [r("поздно", day + 86_400 + 1_800), r("середина", day + 45_000), r("завтра", day + 90_000)];
        let m = day_marks(&list, day, len);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0], (0.5, "середина".to_string()));
        assert_eq!(m[1], (0.98, "поздно".to_string()));
    }
    #[test]
    fn apply_city_sets_all() {
        let mut cfg = store::Settings::default();
        cfg.city = "Старый".into();
        apply_city(&mut cfg, "Минден", 52.3, 8.9, "Minden, DE");
        assert_eq!(cfg.city, "Минден");
        assert_eq!(cfg.lat, Some(52.3));
        assert_eq!(cfg.lon, Some(8.9));
        assert_eq!(cfg.city_name, "Minden, DE");
        assert!(cfg.city_checked);
    }
    #[test]
    fn russian_plural() {
        assert_eq!(dela_word(1), "1 дело");
        assert_eq!(dela_word(3), "3 дела");
        assert_eq!(dela_word(5), "5 дел");
        assert_eq!(dela_word(11), "11 дел");
        assert_eq!(dela_word(12), "12 дел");
        assert_eq!(dela_word(21), "21 дело");
        assert_eq!(dela_word(104), "104 дела");
        assert_eq!(dela_word(0), "0 дел");
    }
}

#[cfg(test)]
mod moon_tests {
    use super::*;
    fn d(y: i32, m: u32, day: u32) -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }
    #[test]
    fn moon_and_day() {
        // известные даты: новолуние 8 апреля 2024, полнолуние 23 апреля 2024
        let new = moon_age(d(2024, 4, 8));
        assert!(new < 1.2 || new > LUNAR - 1.2, "новолуние: {new}");
        let full = moon_age(d(2024, 4, 23));
        assert!((full - 14.8).abs() < 1.2, "полнолуние: {full}");
        assert_eq!(moon_phase(0.2).1, "Новолуние");
        assert_eq!(moon_phase(10.0).1, "Растущая луна");
        assert!(moon_phase(14.77).0 > 0.99);
        assert_eq!(moon_phase(14.77).3, 0);
        assert_eq!(moon_phase(15.0).3, 0);
        assert_eq!(moon_phase(15.4).3, 29);
        assert_eq!(moon_phase(10.0).3, 5);
        assert!(!moon_phase(20.0).2);
        assert_eq!(day_length(7.5, 16.25), 525);
        assert_eq!(day_length(9.0, 8.0), 0);
    }
}

#[cfg(test)]
mod sky_tests {
    use super::*;
    #[test]
    fn phases() {
        assert_eq!(day_phase(7.5, 7.8, 16.5), 0);
        assert_eq!(day_phase(12.0, 7.8, 16.5), 1);
        assert_eq!(day_phase(16.0, 7.8, 16.5), 2);
        assert_eq!(day_phase(2.5, 7.8, 16.5), 3);
        assert_eq!(day_phase(22.0, 7.8, 16.5), 3);
    }
}

/// Обложка-заглушка: диагональный градиент двух соседних оттенков, выбранных по названию трека,
/// мягкий блик сверху и нота. У каждого трека свой цвет, но всегда в спокойной гамме.
fn art_placeholder(p: &egui::Painter, rect: Rect, r: f32, seed: &str, glyph: f32) {
    let h = seed.bytes().fold(2166136261u32, |a, b| (a ^ b as u32).wrapping_mul(16777619));
    let base = (h % 360) as f32 / 360.0;
    let soft = |c: Color32, k: f32| mix(Color32::from_rgb(24, 22, 40), c, k);
    // зелёно-жёлтые оттенки на тёмном выглядят кисло — сдвигаем их к бирюзе
    let base = if (0.12..0.30).contains(&base) { base + 0.25 } else { base };
    let c1 = soft(hue(base, 1.0), 0.52);
    let c2 = soft(hue(base + 0.11, 1.0), 0.30);
    let pts = contour(rect, r.min(rect.width() / 2.0), 0.0, 8);
    let mut mesh = egui::Mesh::default();
    let diag = (rect.width() + rect.height()).max(1.0);
    // диагональ от первого цвета ко второму плюс мягкое высветление к верхнему краю
    let col = |q: Pos2| {
        let up = 1.0 - ((q.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
        mix(mix(c1, c2, ((q.x - rect.left()) + (q.y - rect.top())) / diag), Color32::WHITE, 0.10 * up * up)
    };
    mesh.colored_vertex(rect.center(), col(rect.center()));
    for (q, _) in &pts {
        mesh.colored_vertex(*q, col(*q));
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    p.add(egui::Shape::mesh(mesh));
    p.rect_stroke(rect, Rounding::same(r), Stroke::new(1.0_f32, rgba(255, 255, 255, 18)));
    p.text(rect.center() + vec2(1.0, 2.0), Align2::CENTER_CENTER, "\u{E8D6}", FontId::proportional(glyph), rgba(0, 0, 0, 60));
    p.text(rect.center(), Align2::CENTER_CENTER, "\u{E8D6}", FontId::proportional(glyph), rgba(255, 255, 255, 225));
}

fn hue(h: f32, a: f32) -> Color32 {
    let h = h.rem_euclid(1.0) * 6.0;
    let ch = |n: f32| {
        let k = (n + h) % 6.0;
        ((1.0 - k.min(4.0 - k).clamp(0.0, 1.0)) * 255.0) as u8
    };
    rgba(ch(5.0), ch(3.0), ch(1.0), (a.clamp(0.0, 1.0) * 255.0) as u8)
}

/// Адрес чата; вопрос подставляется там, где сайт это поддерживает.
fn site_url(site: &str, question: &str) -> String {
    let q = weather::encode(question.trim());
    match site {
        "gemini" => "https://gemini.google.com/app".to_string(),
        "chatgpt" if q.is_empty() => "https://chatgpt.com/".to_string(),
        "chatgpt" => format!("https://chatgpt.com/?q={q}"),
        _ if q.is_empty() => "https://claude.ai/new".to_string(),
        _ => format!("https://claude.ai/new?q={q}"),
    }
}

/// Плавное замедление к концу: быстро стартует, мягко останавливается.
fn ease(x: f32) -> f32 {
    1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(3)
}

/// Упругая кнопка: подсвечивается и чуть растёт под курсором, пружинит при нажатии.
fn jelly(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    jelly_sized(ui, text, None)
}

fn jelly_sized(ui: &mut egui::Ui, text: impl Into<String>, size: Option<Vec2>) -> egui::Response {
    let galley = ui.painter().layout_no_wrap(text.into(), semi(13.0), FG);
    let want = size.unwrap_or(galley.size() + vec2(26.0, 14.0));
    let (rect, resp) = ui.allocate_exact_size(want, Sense::click());
    let hover = ui.ctx().animate_bool_with_time(resp.id.with("hover"), resp.hovered(), 0.12);
    let down = ui.ctx().animate_bool_with_time(resp.id.with("down"), resp.is_pointer_button_down_on(), 0.06);
    if ui.is_rect_visible(rect) {
        let r = Rect::from_center_size(rect.center(), rect.size() * (1.0 - 0.03 * down));
        glass_chip(ui.painter(), r, 11.0f32.min(r.height() / 2.0), 0.55 + 0.45 * hover + 0.25 * down);
        ui.painter().galley(r.center() - galley.size() / 2.0, galley, FG);
    }
    resp
}

/// Глаз персонажа: белок, зрачок с двумя бликами, моргание и варианты для эмоций.
struct Face<'a> {
    p: &'a egui::Painter,
    emo: Emotion,
    look: Vec2,
    blink: f32,
    /// середина персонажа по горизонтали — чтобы брови сходились к центру
    mid: f32,
}

impl Face<'_> {
    fn arc(&self, a: Pos2, b: Pos2, bulge: f32, color: Color32, width: f32) {
        let ctrl = pos2((a.x + b.x) / 2.0, a.y + bulge * 2.0);
        self.p.add(egui::epaint::QuadraticBezierShape::from_points_stroke([a, ctrl, b], false, Color32::TRANSPARENT, Stroke::new(width, color)));
    }

    fn eye(&self, x: f32, y: f32, rx: f32, ry: f32, white: Color32, pupil: Color32, small: bool) {
        let p = self.p;
        let ell = |c: Pos2, r: Vec2, color: Color32| {
            p.add(egui::Shape::ellipse_filled(c, r, color));
        };
        match self.emo {
            Emotion::Love => {
                p.text(pos2(x, y + 0.4), Align2::CENTER_CENTER, "♥", FontId::proportional(rx * 2.6), Color32::from_rgb(255, 82, 110));
            }
            Emotion::Surprised => {
                ell(pos2(x, y), vec2(rx * 1.05, ry * 1.12), white);
                ell(pos2(x, y), vec2(rx * 0.3, rx * 0.3), pupil);
            }
            Emotion::Angry => {
                let side = if x < self.mid { -1.0 } else { 1.0 };
                ell(pos2(x, y + 0.6), vec2(rx, ry * 0.8), white);
                ell(pos2(x - side * 0.8, y + 1.0), vec2(rx * 0.6, rx * 0.6), pupil);
                p.line_segment([pos2(x - side * rx * 1.05, y - ry * 0.55), pos2(x + side * rx * 0.95, y - ry * 1.3)], Stroke::new(if small { 1.1_f32 } else { 1.5 }, white));
            }
            Emotion::Sleepy => self.arc(pos2(x - rx * 0.8, y + 1.0), pos2(x + rx * 0.8, y + 1.0), rx * 0.5, white, if small { 1.2 } else { 1.6 }),
            Emotion::Happy => self.arc(pos2(x - rx * 0.8, y + 1.5), pos2(x + rx * 0.8, y + 1.5), -rx * 0.9, white, if small { 1.3 } else { 1.8 }),
            Emotion::Normal | Emotion::Cool => {
                // моргание сплющивает глаз и чуть расширяет его — мягче, чем резкая смена
                let open = 1.0 - self.blink;
                ell(pos2(x, y + (1.0 - open) * ry * 0.35), vec2(rx * (1.0 + (1.0 - open) * 0.12), (ry * open).max(0.6)), white);
                if open < 0.4 {
                    return;
                }
                let pr = rx * if small { 0.66 } else { 0.64 };
                let c = pos2(x + self.look.x * (rx - pr) * 0.9, y + self.look.y * (ry - pr) * 0.85);
                ell(c, vec2(pr, pr), pupil);
                let shine = if small { pupil } else { Color32::WHITE };
                if !small {
                    ell(pos2(c.x + pr * 0.32, c.y - pr * 0.36), vec2(pr * 0.36, pr * 0.36), shine);
                    ell(pos2(c.x - pr * 0.38, c.y + pr * 0.38), vec2(pr * 0.15, pr * 0.15), shine);
                }
            }
        }
    }
}

/// Круглый цветной значок с переливом сверху вниз — как у действий в Atoll.
fn icon_disc(p: &egui::Painter, center: Pos2, radius: f32, top: [u8; 3], bottom: [u8; 3], glyph: &str, size: f32) {
    use egui::epaint::{Vertex, WHITE_UV};
    const N: u32 = 28;
    let at = |y: f32| {
        let k = ((y - (center.y - radius)) / (2.0 * radius)).clamp(0.0, 1.0);
        let ch = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * k) as u8;
        Color32::from_rgb(ch(top[0], bottom[0]), ch(top[1], bottom[1]), ch(top[2], bottom[2]))
    };
    let mut mesh = egui::Mesh::default();
    mesh.vertices.push(Vertex { pos: center, uv: WHITE_UV, color: at(center.y) });
    for i in 0..N {
        let a = i as f32 / N as f32 * std::f32::consts::TAU;
        let pos = center + vec2(a.cos(), a.sin()) * radius;
        mesh.vertices.push(Vertex { pos, uv: WHITE_UV, color: at(pos.y) });
    }
    for i in 0..N {
        mesh.indices.extend_from_slice(&[0, 1 + i, 1 + (i + 1) % N]);
    }
    p.add(egui::Shape::mesh(mesh));
    p.circle_stroke(center, radius, Stroke::new(1.0_f32, rgba(255, 255, 255, 40)));
    p.text(center, Align2::CENTER_CENTER, glyph, FontId::proportional(size), Color32::WHITE);
}

/// Маленькая стеклянная капсула — кнопка, карточка, выбранная вкладка. k — яркость (0…1.3).
fn glass_chip(p: &egui::Painter, r: Rect, radius: f32, k: f32) {
    use egui::epaint::{Vertex, WHITE_UV};
    const N: usize = 6;
    let white = |a: f32| rgba(255, 255, 255, (a.clamp(0.0, 1.0) * 255.0) as u8);
    let edge = contour(r, radius, 0.0, N);
    let inner = contour(r, radius, 1.2f32.min(radius - 0.3), N);
    let m = edge.len() as u32;
    let mut mesh = egui::Mesh::default();
    let fill = |y: f32| white((0.13 - 0.07 * ((y - r.top()) / r.height()).clamp(0.0, 1.0)) * k);
    for (pos, _) in &edge {
        mesh.vertices.push(Vertex { pos: *pos, uv: WHITE_UV, color: fill(pos.y) });
    }
    mesh.vertices.push(Vertex { pos: r.center(), uv: WHITE_UV, color: fill(r.center().y) });
    for i in 0..m {
        mesh.indices.extend_from_slice(&[i, (i + 1) % m, m]);
    }
    let base = mesh.vertices.len() as u32;
    for i in 0..edge.len() {
        let n = edge[i].1;
        let a = (0.10 + 0.26 * (-n.y).max(0.0) + 0.07 * n.y.max(0.0)) * (0.6 + 0.5 * k);
        mesh.vertices.push(Vertex { pos: edge[i].0, uv: WHITE_UV, color: white(a) });
        mesh.vertices.push(Vertex { pos: inner[i].0, uv: WHITE_UV, color: Color32::TRANSPARENT });
    }
    for i in 0..m {
        let (a, b) = (base + i * 2, base + (i + 1) % m * 2);
        mesh.indices.extend_from_slice(&[a, b, a + 1, b, b + 1, a + 1]);
    }
    p.add(egui::Shape::mesh(mesh));
}

/// Тонкий ползунок: дорожка утолщается под курсором, значение меняется перетаскиванием и колесом мыши.
fn slim_slider(ui: &egui::Ui, id: Id, r: Rect, v: &mut f32, color: Color32, off: bool) -> bool {
    let resp = ui.interact(r.expand2(vec2(4.0, 3.0)), id, Sense::click_and_drag());
    let mut changed = false;
    if resp.is_pointer_button_down_on() || resp.dragged() {
        // ползунок нулевой ширины дал бы NaN вместо громкости
        if let Some(p) = resp.interact_pointer_pos().filter(|_| r.width() > 0.0 && r.width().is_finite()) {
            let nv = ((p.x - r.left()) / r.width()).clamp(0.0, 1.0);
            if (nv - *v).abs() > 0.002 {
                *v = nv;
                changed = true;
            }
        }
    } else if resp.hovered() {
        let wheel = ui.input(|i| i.raw_scroll_delta.y);
        if wheel != 0.0 {
            *v = (*v + wheel.signum() * 0.02).clamp(0.0, 1.0);
            changed = true;
        }
    }
    let hot = ui.ctx().animate_bool_with_time(id.with("hot"), resp.hovered() || resp.dragged(), 0.10);
    let h = 5.0 + 3.0 * hot;
    let track = Rect::from_center_size(r.center(), vec2(r.width(), h));
    let p = ui.painter();
    p.rect_filled(track, h / 2.0, rgba(255, 255, 255, 28));
    let end = track.left() + (track.width() * *v).max(h);
    let fill = if off { rgba(255, 255, 255, 80) } else { color };
    p.rect_filled(Rect::from_min_max(track.min, pos2(end, track.bottom())), h / 2.0, fill);
    p.circle_filled(pos2(end - h / 2.0, track.center().y), 3.5 + 3.0 * hot, Color32::WHITE);
    changed
}

/// Что остров предлагает сделать со скопированным текстом.
#[derive(Clone)]
enum HintAct {
    Open(String),
    Copy(String),
}

#[derive(Clone)]
struct Hint {
    glyph: &'static str,
    label: String,
    color: [u8; 3],
    act: HintAct,
}

/// «Умный буфер»: узнаёт в скопированном ссылку, почту, цвет, путь к файлу или пример и подбирает действие.
/// Ничего не открывает сам — только по щелчку.
fn smart_hint(text: &str) -> Option<Hint> {
    let t = text.trim().trim_matches('"');
    if t.is_empty() || t.len() > 600 || t.contains('\n') {
        return None;
    }
    let solid = !t.contains(char::is_whitespace);
    let low = t.to_lowercase();
    if solid && (low.starts_with("http://") || low.starts_with("https://") || low.starts_with("www.")) {
        let url = if low.starts_with("www.") { format!("https://{t}") } else { t.to_string() };
        return Some(Hint { glyph: "\u{E71B}", label: "Открыть ссылку".into(), color: [0x5A, 0xAB, 0xFF], act: HintAct::Open(url) });
    }
    if solid
        && t.matches('@').count() == 1
        && !t.starts_with('@')
        && t.rsplit('@').next().is_some_and(|d| d.contains('.') && !d.ends_with('.') && !d.starts_with('.'))
        && t.chars().all(|c| c.is_ascii_alphanumeric() || "@._-+".contains(c))
    {
        return Some(Hint { glyph: "\u{E715}", label: "Написать письмо".into(), color: [0x7B, 0xE0, 0x9A], act: HintAct::Open(format!("mailto:{t}")) });
    }
    if let Some(hex) = t.strip_prefix('#') {
        if (hex.len() == 3 || hex.len() == 6) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            let full: String = if hex.len() == 3 { hex.chars().flat_map(|c| [c, c]).collect() } else { hex.to_string() };
            let ch = |i: usize| u8::from_str_radix(&full[i..i + 2], 16).unwrap_or(0);
            let (r, g, b) = (ch(0), ch(2), ch(4));
            let rgb = format!("rgb({r}, {g}, {b})");
            return Some(Hint { glyph: "\u{EF3C}", label: rgb.clone(), color: [r.max(70), g.max(70), b.max(70)], act: HintAct::Copy(rgb) });
        }
    }
    let bytes = t.as_bytes();
    if t.len() < 260 && bytes.len() > 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\' {
        let path = std::path::Path::new(t);
        if path.exists() {
            let dir = if path.is_dir() { Some(path) } else { path.parent() };
            if let Some(dir) = dir {
                return Some(Hint { glyph: "\u{E8B7}", label: "Открыть папку".into(), color: [0xFF, 0xD0, 0x60], act: HintAct::Open(dir.display().to_string()) });
            }
        }
    }
    // пример: только цифры и знаки, и знак действия стоит после первой цифры (номера телефонов и даты не считаем)
    if t.chars().all(|c| c.is_ascii_digit() || " .,+-*/^()".contains(c)) {
        if let Some(first) = t.find(|c: char| c.is_ascii_digit()) {
            let rest = &t[first..];
            if rest.contains(['+', '*', '/', '^']) || rest.contains(" - ") {
                if let Some(v) = sys::eval(&t.replace(',', ".")).filter(|v| v.is_finite()) {
                    let res = fmt_num(v);
                    return Some(Hint { glyph: "\u{E8EF}", label: format!("= {res}"), color: [0xB9, 0xA4, 0xFF], act: HintAct::Copy(res) });
                }
            }
        }
    }
    // «ghbdtn, rfr ltkf?» — русский текст в английской раскладке: гласных почти нет
    if wrong_layout(t) {
        // аккуратный перевод раскладки из «Буфера»: запятая после слова остаётся запятой
        let fixed = clip_transform(ClipTx::Layout, t);
        return Some(Hint { glyph: "\u{E8D3}", label: format!("→ {}", fixed.chars().take(28).collect::<String>()), color: [0x7B, 0xE0, 0x9A], act: HintAct::Copy(fixed) });
    }
    if foreign_phrase(t) {
        let url = format!("https://translate.google.com/?sl=auto&tl=ru&op=translate&text={}", weather::encode(t));
        return Some(Hint { glyph: "\u{E774}", label: "Перевести".into(), color: [0x7F, 0xC8, 0xFF], act: HintAct::Open(url) });
    }
    None
}

/// Переводит текст, набранный не в той раскладке: ЙЦУКЕН ↔ QWERTY по положению клавиш.
/// Направление выбирается по первой букве.
fn swap_layout(s: &str) -> String {
    const EN: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`";
    const RU: &str = "йцукенгшщзхъфывапролджэячсмитьбюё";
    let (en, ru): (Vec<char>, Vec<char>) = (EN.chars().collect(), RU.chars().collect());
    let to_en = s.chars().find(|c| c.is_alphabetic()).is_some_and(|c| ru.contains(&c.to_lowercase().next().unwrap_or(c)));
    s.chars()
        .map(|c| {
            let l = c.to_lowercase().next().unwrap_or(c);
            let (from, to) = if to_en { (&ru, &en) } else { (&en, &ru) };
            from.iter().position(|x| *x == l).map_or(c, |i| to[i])
        })
        .collect()
}

/// Латиница, в которой почти нет гласных, — скорее всего русский текст, набранный в английской раскладке.
fn wrong_layout(t: &str) -> bool {
    let letters: Vec<char> = t.chars().filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_lowercase()).collect();
    if letters.len() < 4 || t.chars().any(|c| ('\u{400}'..='\u{4FF}').contains(&c)) || t.contains("://") || t.contains('@') {
        return false;
    }
    let vowels = letters.iter().filter(|c| "aeiouy".contains(**c)).count();
    vowels * 100 / letters.len() < 15 && swap_layout(t).chars().filter(|c| ('\u{400}'..='\u{4FF}').contains(c)).count() >= letters.len()
}

/// Фраза на иностранном языке, которую стоит предложить перевести: хотя бы два слова, почти все буквы
/// латинские, кириллицы нет, и это не похоже на код или команду.
fn foreign_phrase(t: &str) -> bool {
    let words = t.split_whitespace().filter(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 2).count();
    let letters = t.chars().filter(|c| c.is_alphabetic()).count();
    let latin = t.chars().filter(|c| c.is_ascii_alphabetic() || ('\u{C0}'..='\u{24F}').contains(c)).count();
    let cyr = t.chars().any(|c| ('\u{400}'..='\u{4FF}').contains(&c));
    let code = t.chars().filter(|c| "{}[]<>=;$\\|`_".contains(*c)).count();
    words >= 2 && letters >= 6 && !cyr && latin * 10 >= letters * 9 && code == 0 && letters * 2 >= t.chars().count()
}

#[cfg(test)]
mod hint_tests {
    use super::*;
    #[test]
    fn translate_hint() {
        assert!(foreign_phrase("Hello, how are you?"));
        assert!(foreign_phrase("Où est la bibliothèque"));
        assert!(!foreign_phrase("Привет, как дела"));
        assert!(!foreign_phrase("hello"));
        assert!(!foreign_phrase("let x = vec![1, 2];"));
        assert!(!foreign_phrase("cargo build --release_mode"));
        assert!(!foreign_phrase("12 34 56 ab"));
        assert!(matches!(smart_hint("good morning everyone").map(|h| h.label), Some(l) if l == "Перевести"));
        assert!(wrong_layout("ghbdtn, rfr ltkf?"));
        assert!(!wrong_layout("Hello, how are you?"));
        assert!(!wrong_layout("https://x.org"));
        assert!(matches!(smart_hint("ghbdtn, rfr ltkf?").map(|h| h.label), Some(l) if l.starts_with("→ привет, как")));
        assert_eq!(swap_layout("тщеузфв"), "notepad");
        assert_eq!(swap_layout("ghbdtn"), "привет");
        assert_eq!(swap_layout("Ntktuhfv"), "телеграм");
        assert_eq!(swap_layout("123"), "123");
        // ссылки и почта по-прежнему узнаются первыми
        assert!(matches!(smart_hint("https://example.org").map(|h| h.label), Some(l) if l == "Открыть ссылку"));
    }
}

/// Кольцо-шкала: тусклая дорожка и цветная дуга от верхней точки по часовой стрелке.
fn ring(p: &egui::Painter, c: Pos2, r: f32, w: f32, frac: f32, color: Color32) {
    p.circle_stroke(c, r, Stroke::new(w, rgba(255, 255, 255, 26)));
    let frac = frac.clamp(0.0, 1.0);
    if frac <= 0.002 {
        return;
    }
    let steps = (frac * 72.0).ceil().max(2.0) as usize;
    let pts: Vec<Pos2> = (0..=steps)
        .map(|s| {
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * frac * s as f32 / steps as f32;
            c + vec2(a.cos(), a.sin()) * r
        })
        .collect();
    let (first, last) = (pts[0], pts[steps]);
    p.add(egui::Shape::line(pts, Stroke::new(w, color)));
    p.circle_filled(first, w / 2.0, color);
    p.circle_filled(last, w / 2.0, color);
}

/// Пустая страница: значок в кружке, заголовок и подсказка, что сделать.
fn empty_at(p: &egui::Painter, center: Pos2, glyph: &str, title: &str, hint: &str) {
    let c = center - vec2(0.0, 26.0);
    p.circle_filled(c, 24.0, rgba(255, 255, 255, 16));
    p.text(c, Align2::CENTER_CENTER, glyph, FontId::proportional(20.0), DIM);
    p.text(c + vec2(0.0, 42.0), Align2::CENTER_CENTER, title, semi(14.0), FG);
    p.text(c + vec2(0.0, 62.0), Align2::CENTER_CENTER, hint, FontId::proportional(12.0), DIM);
}

fn empty_state(ui: &mut egui::Ui, glyph: &str, title: &str, hint: &str) {
    ui.add_space(14.0);
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 120.0), Sense::hover());
    empty_at(ui.painter(), r.center(), glyph, title, hint);
}

/// Эквалайзер: столбики от нижнего края области. alpha — яркость, fill — какую долю высоты занимает самый громкий.
fn spectrum(p: &egui::Painter, area: Rect, bars: &[f32], color: Color32, alpha: f32, fill: f32) {
    let n = bars.len() as f32;
    let step = area.width() / n;
    let w = (step * 0.62).max(2.0);
    for (i, v) in bars.iter().enumerate() {
        let h = (3.0 + (area.height() * fill - 3.0) * v.powf(1.3)).min(area.height());
        let x = area.left() + step * (i as f32 + 0.5) - w / 2.0;
        let r = Rect::from_min_max(pos2(x, area.bottom() - h), pos2(x + w, area.bottom()));
        // громкие столбики ярче тихих
        let a = (255.0 * alpha * (0.45 + 0.55 * v)).min(255.0) as u8;
        p.rect_filled(r, Rounding { nw: w / 2.0, ne: w / 2.0, sw: 0.0, se: 0.0 }, Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a));
    }
}

/// Эквалайзер, отражённый от средней линии: столбики растут вверх и вниз. Когда плеер играет,
/// а настоящий звук не слышен (например, звук идёт в наушники мимо устройства), столбики мягко дышат.
fn spectrum_mirror(p: &egui::Painter, area: Rect, bars: &[f32], color: Color32, time: f32, playing: bool) {
    // полос мало (16), поэтому рисуем втрое больше тонких столбиков, плавно переходя между полосами
    const M: usize = 48;
    let step = area.width() / M as f32;
    let w = (step * 0.42).clamp(2.0, 7.0);
    let mid = area.center().y;
    let band = |f: f32| -> f32 {
        if bars.is_empty() {
            return 0.0;
        }
        let x = f * (bars.len() - 1) as f32;
        let (a, k) = (x.floor() as usize, x.fract());
        let b = (a + 1).min(bars.len() - 1);
        bars[a] * (1.0 - k) + bars[b] * k
    };
    for i in 0..M {
        let idle = if playing { 0.10 + 0.08 * (time * 2.1 + i as f32 * 0.35).sin().abs() } else { 0.03 };
        let v = band(i as f32 / (M - 1) as f32).max(idle);
        let h = (area.height() / 2.0 * v.powf(1.1)).max(1.5);
        let x = area.left() + step * (i as f32 + 0.5) - w / 2.0;
        let r = Rect::from_min_max(pos2(x, mid - h), pos2(x + w, mid + h));
        let a = (60.0 + 170.0 * v).min(235.0) as u8;
        p.rect_filled(r, Rounding::same(w / 2.0), Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a));
        // отражение тусклее: нижняя половина приглушена
        p.rect_filled(Rect::from_min_max(pos2(x, mid), pos2(x + w, mid + h)), Rounding { nw: 0.0, ne: 0.0, sw: w / 2.0, se: w / 2.0 }, Color32::from_rgba_unmultiplied(20, 18, 34, 90));
    }
}

/// Строка настройки с переключателем справа; щёлкать можно по всей строке. Возвращает true, если переключили.
fn switch_row(ui: &mut egui::Ui, on: &mut bool, title: &str, hint: &str) -> bool {
    let h = if hint.is_empty() { 30.0 } else { 40.0 };
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    if resp.clicked() {
        *on = !*on;
    }
    let k = ui.ctx().animate_bool_with_time(resp.id.with("on"), *on, 0.14);
    let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
    let p = ui.painter();
    if hov > 0.0 {
        p.rect_filled(r.expand2(vec2(6.0, 0.0)), Rounding::same(9.0), rgba(255, 255, 255, (12.0 * hov) as u8));
    }
    let text = p.with_clip_rect(Rect::from_min_max(r.min, pos2(r.right() - 48.0, r.bottom())));
    if hint.is_empty() {
        text.text(pos2(r.left(), r.center().y), Align2::LEFT_CENTER, title, FontId::proportional(13.0), FG);
    } else {
        text.text(pos2(r.left(), r.top() + 12.0), Align2::LEFT_CENTER, title, FontId::proportional(13.0), FG);
        text.text(pos2(r.left(), r.top() + 29.0), Align2::LEFT_CENTER, hint, FontId::proportional(11.0), DIM);
    }
    let track = Rect::from_min_size(pos2(r.right() - 38.0, r.center().y - 10.0), vec2(36.0, 20.0));
    let a = accent();
    p.rect_filled(track, Rounding::same(10.0), rgba(255, 255, 255, 38));
    if k > 0.0 {
        p.rect_filled(track, Rounding::same(10.0), Color32::from_rgba_unmultiplied(a.r(), a.g(), a.b(), (k * 255.0) as u8));
    }
    p.circle_filled(pos2(track.left() + 10.0 + 16.0 * k, track.center().y), 7.5, Color32::WHITE);
    resp.clicked()
}

/// Смешение двух непрозрачных цветов.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
}

/// Как mix, но сохраняет прозрачность: смешивает и цвет, и непрозрачность (в egui цвета хранятся
/// уже умноженными на прозрачность, поэтому каналы можно смешивать напрямую).
fn mix_a(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()), ch(a.a(), b.a()))
}

fn rounded_image(p: &egui::Painter, tex: &egui::TextureHandle, rect: Rect, radius: f32) {
    let mut s = egui::epaint::RectShape::filled(rect, Rounding::same(radius), Color32::WHITE);
    s.fill_texture_id = tex.id();
    s.uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    p.add(s);
}

fn fmt_left(end: Instant) -> String {
    fmt_secs(end.saturating_duration_since(Instant::now()).as_secs_f64().ceil() as u64)
}

/// «02:05» или «1:02:05».
fn fmt_secs(s: u64) -> String {
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

fn fmt_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 && v.abs() < 1e15 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Полужирный Segoe UI — заголовки, вкладки, кнопки, часы.
fn semi(size: f32) -> FontId {
    FontId::new(size, egui::FontFamily::Name("semi".into()))
}

/// Светлый Segoe UI — крупные цифры.
fn light(size: f32) -> FontId {
    FontId::new(size, egui::FontFamily::Name("light".into()))
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let dir = format!("{}/Fonts", std::env::var("WINDIR").unwrap_or_else(|_| "C:/Windows".into()));
    let mut have = std::collections::HashSet::new();
    for (name, file) in [
        ("segoe", "segoeui.ttf"),
        ("segoe-semi", "seguisb.ttf"),
        ("segoe-light", "segoeuil.ttf"),
        ("symbols", "seguisym.ttf"),
        ("icons", "segmdl2.ttf"),
    ] {
        if let Ok(bytes) = std::fs::read(format!("{dir}/{file}")) {
            fonts.font_data.insert(name.to_string(), egui::FontData::from_owned(bytes));
            have.insert(name);
        }
    }
    // у каждого начертания один и тот же хвост: символы, значки, затем встроенные шрифты
    let fallback: Vec<String> = fonts.families.get(&egui::FontFamily::Proportional).cloned().unwrap_or_default();
    let chain = |first: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = first.iter().chain(["symbols", "icons"].iter()).filter(|n| have.contains(*n)).map(|n| n.to_string()).collect();
        v.extend(fallback.iter().cloned());
        v
    };
    fonts.families.insert(egui::FontFamily::Proportional, chain(&["segoe"]));
    fonts.families.insert(egui::FontFamily::Name("semi".into()), chain(&["segoe-semi", "segoe"]));
    fonts.families.insert(egui::FontFamily::Name("light".into()), chain(&["segoe-light", "segoe"]));
    ctx.set_fonts(fonts);
}

fn setup_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = Color32::TRANSPARENT;
    v.window_fill = Color32::TRANSPARENT;
    v.override_text_color = Some(FG);
    v.extreme_bg_color = tint(14);
    v.selection.bg_fill = tint(46);
    v.selection.stroke = Stroke::new(1.0_f32, FG);
    for (w, alpha) in [
        (&mut v.widgets.inactive, 18),
        (&mut v.widgets.hovered, 40),
        (&mut v.widgets.active, 60),
        (&mut v.widgets.open, 40),
    ] {
        w.weak_bg_fill = tint(alpha);
        w.bg_fill = tint(alpha);
        w.bg_stroke = Stroke::NONE;
        w.rounding = Rounding::same(9.0);
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::NONE;
    ctx.set_visuals(v);
    ctx.style_mut(|s| {
        s.spacing.button_padding = vec2(10.0, 5.0);
        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.interaction.selectable_labels = false;
        s.animation_time = 0.12;
        // единая шкала размеров: основной текст 14, подписи 12, заголовки 17
        s.text_styles.insert(egui::TextStyle::Body, FontId::proportional(13.5));
        s.text_styles.insert(egui::TextStyle::Button, semi(13.5));
        s.text_styles.insert(egui::TextStyle::Small, FontId::proportional(12.0));
        s.text_styles.insert(egui::TextStyle::Heading, semi(17.0));
    });
}

fn main() -> eframe::Result<()> {
    let test = test_mode();
    // «Ostrovok.exe --install»: установить и запустить установленную копию без окна настроек
    if std::env::args().any(|a| a == "--install") {
        match install::install() {
            Ok(new) => {
                println!("установлено: {}", new.display());
                if !test {
                    let _ = std::process::Command::new(&new).spawn();
                }
            }
            Err(e) => eprintln!("{e}"),
        }
        return Ok(());
    }
    let updated = std::env::args().any(|a| a == "--updated");
    if !test && !(if updated { sys::wait_single_instance() } else { sys::single_instance() }) {
        return Ok(());
    }
    // мы — единственная копия, значит новая версия сейчас не работает. Если она не прошла испытание
    // (упала в первые секунды), возвращаем прежнюю и запускаем её; та подождёт, пока эта копия выйдет.
    // OSTROVOK_TEST_ROLLBACK — только для проверки отката проверочной копией во временной папке
    let rollback_on = !test || std::env::var_os("OSTROVOK_TEST_ROLLBACK").is_some();
    if rollback_on && !updated && update::rollback_if_failed() {
        return Ok(());
    }
    let test_x = std::env::var("OSTROVOK_TEST_X").ok().and_then(|v| v.parse::<f32>().ok());
    let x = if let Some(tx) = test_x { tx } else if test { -6000.0 } else { ((sys::screen_width() as f32 - WIN_W) / 2.0).max(0.0) };
    // Если сейчас идёт полноэкранная игра, окно создаётся сразу за пределами экрана: окно «поверх всех»,
    // появившееся над игрой хоть на кадр, сворачивает её. На место остров встанет сам, когда игра уйдёт.
    let start_at = if !test && sys::fullscreen_app_active() { [-9000.0, 0.0] } else { [x + WIN_W / 2.0 - 90.0, TOP - 2.0] };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(if test { "Островок (тест)" } else { "Островок" })
        .with_inner_size([180.0, PILL_H + 4.0])
        .with_position(start_at)
        .with_decorations(false)
        .with_transparent(true)
        .with_taskbar(false)
        .with_resizable(false)
        .with_active(false);
    if !test || test_x.is_some() {
        viewport = viewport.with_always_on_top();
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("Островок", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

/// Быстрые преобразования текста на странице «Буфер».
#[derive(Clone, Copy, PartialEq, Debug)]
enum ClipTx {
    Upper,
    Lower,
    OneLine,
    Translit,
    Layout,
}

impl ClipTx {
    const ALL: [(ClipTx, &'static str, &'static str); 5] = [
        (ClipTx::Upper, "ВЕРХНИЙ", "Все буквы заглавные"),
        (ClipTx::Lower, "нижний", "Все буквы строчные"),
        (ClipTx::OneLine, "Одной строкой", "Убрать переносы и лишние пробелы"),
        (ClipTx::Translit, "Транслит", "Русские буквы латиницей: Щука → Shchuka"),
        (ClipTx::Layout, "Раскладка", "Набрано не в той раскладке: ghbdtn → привет"),
    ];
}

/// Размер миниатюры картинки из буфера (длинная сторона 256). Пустая картинка 0×N
/// из чужой программы раньше роняла панель делением на ноль.
fn clip_thumb_size(w: usize, h: usize) -> Option<(u32, u32)> {
    if w == 0 || h == 0 || w > u32::MAX as usize || h > u32::MAX as usize {
        return None;
    }
    let m = w.max(h) as u64;
    Some(((256 * w as u64 / m).max(1) as u32, (256 * h as u64 / m).max(1) as u32))
}

#[cfg(test)]
mod clip_thumb_tests {
    #[test]
    fn empty_image_does_not_panic() {
        assert_eq!(super::clip_thumb_size(0, 0), None);
        assert_eq!(super::clip_thumb_size(0, 10), None);
        assert_eq!(super::clip_thumb_size(1000, 500), Some((256, 128)));
        assert_eq!(super::clip_thumb_size(1, 100000), Some((1, 256)));
    }
}

fn clip_transform(tx: ClipTx, text: &str) -> String {
    match tx {
        ClipTx::Upper => text.to_uppercase(),
        ClipTx::Lower => text.to_lowercase(),
        ClipTx::OneLine => text.split_whitespace().collect::<Vec<_>>().join(" "),
        ClipTx::Translit => translit(text),
        ClipTx::Layout => fix_layout(text),
    }
}

fn translit(text: &str) -> String {
    let cs: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in cs.iter().enumerate() {
        let low = c.to_lowercase().next().unwrap_or(c);
        let lat = match low {
            'а' => "a", 'б' => "b", 'в' => "v", 'г' => "g", 'д' => "d", 'е' => "e", 'ё' => "yo", 'ж' => "zh", 'з' => "z", 'и' => "i", 'й' => "y",
            'к' => "k", 'л' => "l", 'м' => "m", 'н' => "n", 'о' => "o", 'п' => "p", 'р' => "r", 'с' => "s", 'т' => "t", 'у' => "u", 'ф' => "f",
            'х' => "kh", 'ц' => "ts", 'ч' => "ch", 'ш' => "sh", 'щ' => "shch", 'ъ' | 'ь' => "", 'ы' => "y", 'э' => "e", 'ю' => "yu", 'я' => "ya",
            _ => {
                out.push(c);
                continue;
            }
        };
        if c == low {
            out.push_str(lat);
            continue;
        }
        // заглавная: всё слово заглавными (ЖУК → ZHUK) или только первая буква (Щука → Shchuka)
        let next_up = cs.get(i + 1).is_some_and(|n| n.is_uppercase());
        let next_letter = cs.get(i + 1).is_some_and(|n| n.is_alphabetic());
        let prev_up = i > 0 && cs[i - 1].is_uppercase();
        if next_up || (!next_letter && prev_up) {
            out.push_str(&lat.to_uppercase());
        } else {
            let mut l = lat.chars();
            if let Some(f) = l.next() {
                out.extend(f.to_uppercase());
                out.push_str(l.as_str());
            }
        }
    }
    out
}

/// Текст, набранный не в той раскладке: латиницу в кириллицу или наоборот — куда больше букв просится.
fn fix_layout(text: &str) -> String {
    const EN: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`";
    const RU: &str = "йцукенгшщзхъфывапролджэячсмитьбюё";
    let (en, ru): (Vec<char>, Vec<char>) = (EN.chars().collect(), RU.chars().collect());
    let latin = text.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let cyr = text.chars().filter(|c| ru.contains(&c.to_lowercase().next().unwrap_or(*c))).count();
    let to_ru = latin >= cyr;
    let cs: Vec<char> = text.chars().collect();
    cs.iter()
        .enumerate()
        .map(|(i, &c)| {
            let low = c.to_lowercase().next().unwrap_or(c);
            let (from, to) = if to_ru { (&en, &ru) } else { (&ru, &en) };
            let Some(k) = from.iter().position(|f| *f == low) else { return c };
            // знаки препинания меняем только внутри слова: «j,tl» → «обед», а запятая в «ghbdtn, vbh» остаётся
            if to_ru && !low.is_ascii_alphabetic() && !cs.get(i + 1).is_some_and(|n| n.is_ascii_alphabetic()) {
                return c;
            }
            let t = to[k];
            if c != low { t.to_uppercase().next().unwrap_or(t) } else { t }
        })
        .collect()
}

/// Показывать ли на полке миниатюру вместо значка: png/jpg до 40 МБ, файл на месте и это не папка.
fn shelf_thumb_ok(path: &str, size: Option<u64>) -> bool {
    let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg") && size.is_some_and(|b| b <= 40 << 20)
}

/// Размер картинки w×h, вписанной в max_w×max_h с сохранением пропорций.
fn thumb_fit(w: u32, h: u32, max_w: f32, max_h: f32) -> (f32, f32) {
    if w == 0 || h == 0 {
        return (0.0, 0.0);
    }
    let k = (max_w / w as f32).min(max_h / h as f32);
    (w as f32 * k, h as f32 * k)
}

/// Читает картинку и уменьшает до 176 px по большей стороне: (ширина, высота, RGBA).
fn load_thumb(path: &str) -> Option<(usize, usize, Vec<u8>)> {
    // файл может быть чужим: ограничиваем размеры и память, чтобы маленький «раздутый» PNG не съел гигабайты
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(16_000);
    limits.max_image_height = Some(16_000);
    limits.max_alloc = Some(300 << 20);
    let mut reader = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    reader.limits(limits);
    let img = reader.decode().ok()?.thumbnail(176, 176).into_rgba8();
    Some((img.width() as usize, img.height() as usize, img.into_raw()))
}

/// Рамка, протянутая мышью от a до b по миниатюре img, в координатах снимка size: [x0, y0, x1, y1), x1/y1 не включительно.
/// Выход за край обрезается; рамка меньше двух точек миниатюры (просто щелчок) не считается.
fn view_to_image(a: Pos2, b: Pos2, img: Rect, size: (usize, usize)) -> Option<[u32; 4]> {
    let (a, b) = (img.clamp(a), img.clamp(b));
    let r = Rect::from_two_pos(a, b);
    if r.width() < 2.0 || r.height() < 2.0 || img.width() <= 0.0 || img.height() <= 0.0 {
        return None;
    }
    let sx = |x: f32| (((x - img.left()) / img.width()) * size.0 as f32).round().clamp(0.0, size.0 as f32) as u32;
    let sy = |y: f32| (((y - img.top()) / img.height()) * size.1 as f32).round().clamp(0.0, size.1 as f32) as u32;
    Some([sx(r.left()), sy(r.top()), sx(r.right()), sy(r.bottom())])
}

/// PNG без обрезков: пишем во временный файл рядом и только целый переименовываем в итоговый.
/// Не вышло (диск полон, нет прав) — временный файл убираем.
fn save_png_atomic(out: &std::path::Path, px: &[u8], w: usize, h: usize) -> Result<(), String> {
    let (Ok(w32), Ok(h32)) = (u32::try_from(w), u32::try_from(h)) else { return Err("слишком большой снимок".into()) };
    if w == 0 || h == 0 || w.checked_mul(h).and_then(|n| n.checked_mul(4)) != Some(px.len()) {
        return Err("неверный размер снимка".into());
    }
    let tmp = out.with_extension(format!("{}.tmp", std::process::id()));
    let res = image::save_buffer_with_format(&tmp, px, w32, h32, image::ExtendedColorType::Rgba8, image::ImageFormat::Png)
        .map_err(|e| e.to_string())
        .and_then(|_| std::fs::rename(&tmp, out).map_err(|e| e.to_string()));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Куда сохранить размеченную копию снимка: «<имя> (разметка).png» рядом, при занятом — «(разметка 2)» и т.д.
fn marked_path(path: &str, exists: impl Fn(&std::path::Path) -> bool) -> std::path::PathBuf {
    let p = std::path::Path::new(path);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Снимок".into());
    let dir = p.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    (1..1000)
        .map(|n| dir.join(if n == 1 { format!("{stem} (разметка).png") } else { format!("{stem} (разметка {n}).png") }))
        .find(|c| !exists(c))
        .unwrap_or_else(|| dir.join(format!("{stem} (разметка {}).png", chrono::Local::now().timestamp())))
}

/// Рисует прямоугольную рамку толщиной t прямо в RGBA-снимке w×h; всё за краем пропускается.
fn draw_frame_rgba(px: &mut [u8], w: usize, h: usize, r: [u32; 4], t: usize, color: [u8; 3]) {
    // рамку могли получить перевёрнутой или за краями — упорядочиваем и обрезаем по снимку
    let (x0, x1) = ((r[0].min(r[2]) as usize).min(w), (r[0].max(r[2]) as usize).min(w));
    let (y0, y1) = ((r[1].min(r[3]) as usize).min(h), (r[1].max(r[3]) as usize).min(h));
    if px.len() < w.saturating_mul(h).saturating_mul(4) {
        return;
    }
    for y in y0..y1 {
        for x in x0..x1 {
            let edge = x < x0 + t || x >= x1.saturating_sub(t) || y < y0 + t || y >= y1.saturating_sub(t);
            let i = (y * w + x) * 4;
            if edge && i + 4 <= px.len() {
                px[i..i + 4].copy_from_slice(&[color[0], color[1], color[2], 255]);
            }
        }
    }
}

#[cfg(test)]
mod shot_mark_tests {
    use super::*;

    #[test]
    fn view_to_image_scales_and_clamps() {
        // миниатюра 100×50 в точке (10,20) показывает снимок 1000×500
        let img = Rect::from_min_size(pos2(10.0, 20.0), vec2(100.0, 50.0));
        assert_eq!(view_to_image(pos2(20.0, 30.0), pos2(60.0, 45.0), img, (1000, 500)), Some([100, 100, 500, 250]));
        // протянули справа налево и за край — упорядочиваем и обрезаем
        assert_eq!(view_to_image(pos2(200.0, 90.0), pos2(60.0, 0.0), img, (1000, 500)), Some([500, 0, 1000, 500]));
        assert_eq!(view_to_image(pos2(20.0, 30.0), pos2(20.5, 30.2), img, (1000, 500)), None, "просто щелчок — не рамка");
        assert_eq!(view_to_image(pos2(0.0, 0.0), pos2(5.0, 5.0), img, (1000, 500)), None, "целиком мимо снимка");
    }

    #[test]
    fn frame_inverted_outside_and_empty() {
        let (w, h) = (10, 8);
        let count = |px: &[u8]| px.chunks(4).filter(|c| c[0] == 255).count();
        // перевёрнутая рамка рисуется так же, как прямая
        let mut a = vec![0u8; w * h * 4];
        let mut b = a.clone();
        draw_frame_rgba(&mut a, w, h, [1, 1, 6, 5], 1, [255, 0, 0]);
        draw_frame_rgba(&mut b, w, h, [6, 5, 1, 1], 1, [255, 0, 0]);
        assert_eq!(a, b);
        assert_eq!(count(&a), 14);
        // целиком за краями и нулевая — ничего, без паники
        let mut c = vec![0u8; w * h * 4];
        draw_frame_rgba(&mut c, w, h, [50, 50, 4000000000, 4000000000], 3, [255, 0, 0]);
        draw_frame_rgba(&mut c, w, h, [3, 3, 3, 3], 3, [255, 0, 0]);
        assert_eq!(count(&c), 0);
        // буфер короче заявленного размера — тоже без паники
        let mut d = vec![0u8; 10];
        draw_frame_rgba(&mut d, w, h, [0, 0, 10, 8], 2, [255, 0, 0]);
    }

    #[test]
    fn png_saved_whole_or_not_at_all() {
        let dir = std::env::temp_dir().join(format!("ostrovok-png-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("a (разметка).png");
        save_png_atomic(&out, &[255u8; 2 * 2 * 4], 2, 2).unwrap();
        assert_eq!(image::open(&out).unwrap().width(), 2);
        assert!(save_png_atomic(&dir.join("b.png"), &[0u8; 3], 2, 2).is_err());
        assert!(save_png_atomic(&dir.join("c.png"), &[], 0, 0).is_err());
        let bad = dir.join("нет такой папки").join("d.png");
        assert!(save_png_atomic(&bad, &[0u8; 16], 2, 2).is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "временных файлов не осталось");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn marked_copy_name() {
        let none = |_: &std::path::Path| false;
        assert_eq!(marked_path(r"C:\P\Снимок 1.png", none), std::path::PathBuf::from(r"C:\P\Снимок 1 (разметка).png"));
        let taken = |p: &std::path::Path| {
            let s = p.to_string_lossy();
            s.ends_with("(разметка).png") || s.ends_with("(разметка 2).png")
        };
        assert_eq!(marked_path(r"C:\P\a.png", taken), std::path::PathBuf::from(r"C:\P\a (разметка 3).png"));
        assert_eq!(marked_path(r"C:\P\b", none), std::path::PathBuf::from(r"C:\P\b (разметка).png"), "без расширения — png");
    }

    #[test]
    fn frame_drawn_only_on_border() {
        let (w, h) = (10usize, 8usize);
        let mut px = vec![0u8; w * h * 4];
        draw_frame_rgba(&mut px, w, h, [2, 2, 8, 6], 1, [255, 0, 0]);
        let at = |px: &[u8], x: usize, y: usize| px[(y * w + x) * 4..(y * w + x) * 4 + 4].to_vec();
        assert_eq!(at(&px, 2, 2), [255, 0, 0, 255]);
        assert_eq!(at(&px, 7, 5), [255, 0, 0, 255]);
        assert_eq!(at(&px, 5, 2), [255, 0, 0, 255]);
        assert_eq!(at(&px, 4, 4), [0, 0, 0, 0], "внутри пусто");
        assert_eq!(at(&px, 9, 7), [0, 0, 0, 0], "снаружи не тронуто");
        // толщина больше рамки и выход за край не роняют программу
        draw_frame_rgba(&mut px, w, h, [0, 0, 99, 99], 50, [0, 255, 0]);
        assert_eq!(at(&px, 9, 7), [0, 255, 0, 255]);
    }
}

#[cfg(test)]
mod recent_tests {
    use super::*;

    #[test]
    fn push_dedup_cap() {
        let mut r = Vec::new();
        for q in ["таймер 5", "notepad", "100 usd в рублях", "время токио", "до нового года", "калькулятор"] {
            push_recent(&mut r, q);
        }
        assert_eq!(r, vec!["калькулятор", "до нового года", "время токио", "100 usd в рублях", "notepad"]);
        push_recent(&mut r, "  Время Токио ");
        assert_eq!(r[0], "Время Токио", "повтор поднимается наверх, без дубля");
        assert_eq!(r.len(), 5);
        push_recent(&mut r, "   ");
        push_recent(&mut r, &"я".repeat(61));
        assert_eq!(r[0], "Время Токио", "пустое и слишком длинное не сохраняем");
    }

    #[test]
    fn what_is_remembered() {
        assert!(recent_worthy(&Action::Launch("C:\\x.exe".into())));
        assert!(recent_worthy(&Action::Url("https://ya.ru".into())));
        assert!(recent_worthy(&Action::Copy("42".into())));
        assert!(recent_worthy(&Action::Timer(5.0)));
        assert!(!recent_worthy(&Action::Ask("вопрос".into())));
        assert!(!recent_worthy(&Action::Quick(Quick::Note("личное".into()))));
        assert!(!recent_worthy(&Action::Quick(Quick::Todo("дело".into()))));
    }
}

#[cfg(test)]
mod inbox_filter_tests {
    use super::*;

    fn it(app: &str, c: u8) -> InboxItem {
        InboxItem { app: app.into(), title: "t".into(), body: String::new(), color: [c, 0, 0], at: chrono::Local::now() }
    }

    #[test]
    fn apps_counted_in_order_of_first_message() {
        let list = [it("Telegram", 1), it("Discord", 2), it("Telegram", 1), it("WhatsApp", 3)];
        assert_eq!(inbox_apps(&list), vec![("Telegram".to_string(), 2, [1, 0, 0]), ("Discord".to_string(), 1, [2, 0, 0]), ("WhatsApp".to_string(), 1, [3, 0, 0])]);
        assert!(inbox_apps(&[]).is_empty());
    }

    #[test]
    fn visible_by_filter() {
        let list = [it("Telegram", 1), it("Discord", 2), it("Telegram", 1)];
        assert_eq!(inbox_visible(&list, None), vec![0, 1, 2]);
        assert_eq!(inbox_visible(&list, Some("Telegram")), vec![0, 2]);
        assert_eq!(inbox_visible(&list, Some("Discord")), vec![1]);
        assert!(inbox_visible(&list, Some("Slack")).is_empty());
    }
}

#[cfg(test)]
mod shelf_thumb_tests {
    use super::*;

    #[test]
    fn which_files_get_thumbs() {
        assert!(shelf_thumb_ok("C:\\Windows\\Web\\Wallpaper\\Theme1\\img1.jpg", Some(500_000)));
        assert!(shelf_thumb_ok("D:\\a\\Снимок.PNG", Some(1)));
        assert!(shelf_thumb_ok("x.jpeg", Some(10)));
        assert!(!shelf_thumb_ok("x.gif", Some(10)));
        assert!(!shelf_thumb_ok("C:\\папка.jpg", Some(u64::MAX)), "папка с «картиночным» именем");
        assert!(!shelf_thumb_ok("x.png", None), "файла нет");
        assert!(!shelf_thumb_ok("x.png", Some(41 << 20)), "слишком большой");
        assert!(!shelf_thumb_ok("png", Some(10)));
    }

    #[test]
    fn fit_keeps_aspect() {
        let near = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3;
        assert!(near(thumb_fit(1920, 1080, 88.0, 46.0), (81.7778, 46.0)));
        assert!(near(thumb_fit(100, 400, 88.0, 46.0), (11.5, 46.0)));
        assert!(near(thumb_fit(4000, 100, 88.0, 46.0), (88.0, 2.2)));
        assert_eq!(thumb_fit(0, 0, 88.0, 46.0), (0.0, 0.0));
    }
}

#[cfg(test)]
mod clip_transform_tests {
    use super::*;

    #[test]
    fn case_and_one_line() {
        assert_eq!(clip_transform(ClipTx::Upper, "Привет, world"), "ПРИВЕТ, WORLD");
        assert_eq!(clip_transform(ClipTx::Lower, "ПРИВЕТ, World"), "привет, world");
        assert_eq!(clip_transform(ClipTx::OneLine, "  первая\r\nвторая\n\n\tтретья  "), "первая вторая третья");
        assert_eq!(clip_transform(ClipTx::OneLine, ""), "");
    }

    #[test]
    fn translit_keeps_case_and_other_chars() {
        assert_eq!(clip_transform(ClipTx::Translit, "Щука ёж, Юля!"), "Shchuka yozh, Yulya!");
        assert_eq!(clip_transform(ClipTx::Translit, "объём 5 GB"), "obyom 5 GB");
        assert_eq!(clip_transform(ClipTx::Translit, "ЖУК"), "ZHUK");
    }

    #[test]
    fn layout_both_ways() {
        assert_eq!(clip_transform(ClipTx::Layout, "ghbdtn, vbh!"), "привет, мир!");
        assert_eq!(clip_transform(ClipTx::Layout, "Ghbdtn"), "Привет");
        assert_eq!(clip_transform(ClipTx::Layout, "руддщ цщкдв"), "hello world");
        assert_eq!(clip_transform(ClipTx::Layout, "123 - !"), "123 - !");
    }
}

#[cfg(test)]
mod cache_lock_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn qr_cache_reuses_and_recomputes() {
        let mut c: Option<QrCache> = None;
        assert!(qr_cached(&mut c, "abc").is_some());
        // подменяем данные: повтор с тем же текстом не должен пересчитывать
        if let Some((_, Some((_, v)))) = c.as_mut() {
            v.clear();
        }
        assert!(qr_cached(&mut c, "abc").unwrap().1.is_empty());
        assert!(!qr_cached(&mut c, "другое").unwrap().1.is_empty());
        let long: String = std::iter::repeat('я').take(3000).collect();
        assert!(qr_cached(&mut c, &long).is_none());
        assert_eq!(c.as_ref().map(|(t, v)| (t == &long, v.is_none())), Some((true, true)));
    }

    #[test]
    fn qr_kind_case_insensitive_and_safe() {
        assert_eq!(qr_kind("HTTPS://A.RU"), "Ссылка");
        assert_eq!(qr_kind("Http://x"), "Ссылка");
        assert_eq!(qr_kind("httpsx"), "Текст");
        assert_eq!(qr_kind("приветмир"), "Текст");
        assert_eq!(qr_kind("пр"), "Текст");
        assert_eq!(qr_kind(""), "Текст");
    }

    #[test]
    fn lock_reads_poisoned_mutex() {
        let m = Arc::new(Mutex::new(5));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("отравляем");
        })
        .join();
        assert!(m.is_poisoned());
        assert_eq!(*lock(&m), 5);
    }

    #[test]
    fn icon_keys_drop_only_over_limit() {
        let keys: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let keep: HashSet<&str> = ["a"].into_iter().collect();
        assert!(icon_keys_to_drop(keys.iter().map(|s| s.as_str()), &keep, 3).is_empty());
        let mut d = icon_keys_to_drop(keys.iter().map(|s| s.as_str()), &keep, 2);
        d.sort();
        assert_eq!(d, vec!["b".to_string(), "c".to_string()]);
    }
}

/// Шаг кадра: мусор (NaN, бесконечность) — как обычный кадр 60 Гц, иначе в разумных пределах.
fn frame_dt(raw: f32) -> f32 {
    if raw.is_finite() { raw.clamp(0.001, 0.05) } else { 1.0 / 60.0 }
}

/// Период фазы волны: phase идёт в sin с множителем 1, а лицо берёт t = phase/7.5
/// с множителями, кратными 0.1, — t повторяется через 20π, фаза через 150π.
const RIPPLE_PERIOD: f32 = 150.0 * std::f32::consts::PI;
/// Период фазы капель: множители 1, 0.7, 2.6, 37 — все кратны 0.1, общий период 20π.
const DRIP_PERIOD: f32 = 20.0 * std::f32::consts::PI;

/// Держит растущую фазу в [0, period), чтобы f32 не терял точность за дни работы.
fn wrap_phase(p: f32, period: f32) -> f32 {
    if !p.is_finite() {
        return 0.0;
    }
    let w = p.rem_euclid(period);
    if w >= period { 0.0 } else { w }
}

#[cfg(test)]
mod frame_tests {
    use super::*;

    #[test]
    fn frame_dt_sane() {
        assert_eq!(frame_dt(0.0), 0.001);
        assert_eq!(frame_dt(-1.0), 0.001);
        assert_eq!(frame_dt(1e9), 0.05);
        assert_eq!(frame_dt(f32::NAN), 1.0 / 60.0);
        assert_eq!(frame_dt(f32::INFINITY), 1.0 / 60.0);
        assert_eq!(frame_dt(0.02), 0.02);
    }

    #[test]
    fn phases_wrap_without_jump() {
        for &(p, m) in &[(RIPPLE_PERIOD, [1.0f32, 1.0 / 7.5 * 2.2, 1.0 / 7.5 * 55.0]), (DRIP_PERIOD, [0.7, 2.6, 37.0])] {
            let before = p - 0.01;
            let after = wrap_phase(before + 0.02, p);
            assert!(after < 0.02 && after >= 0.0);
            for k in m {
                assert!((((before + 0.02) * k).sin() - (after * k).sin()).abs() < 2e-3, "рывок при множителе {k}");
            }
        }
        assert_eq!(wrap_phase(1.0, DRIP_PERIOD), 1.0);
        assert!(wrap_phase(f32::NAN, DRIP_PERIOD) == 0.0);
    }
}
