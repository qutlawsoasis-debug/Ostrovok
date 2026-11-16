#![windows_subsystem = "windows"]
//! Островок — живая панель в верхней части экрана Windows.

mod ai;
mod fx;
mod convert;
mod ears;
mod glass;
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
const WIN_H: f32 = 680.0;
const AI_H: f32 = 620.0;
/// Способ размытия фона по умолчанию: acrylic, blur или off.
const DEFAULT_BLUR: &str = "off";
const TOP: f32 = 6.0;
const PILL_H: f32 = 36.0;
const PANEL_W: f32 = 700.0;
const PANEL_H: f32 = 318.0;
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

fn card() -> Color32 {
    tint(18)
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
    City(f64, f64, String),
    Weather(weather::Now),
    Ai(Result<String, String>),
    Jarvis(jarvis::Ev),
    Heard(ears::Heard),
    /// снимок области пришёл из буфера обмена: ширина, высота, точки
    Shot(usize, usize, Vec<u8>),
    Download(String),
    Notice(live::Notice),
    Lyrics(String, Vec<(f64, String)>),
}

#[derive(Clone)]
enum Action {
    Launch(String),
    Url(String),
    Copy(String),
    Timer(f64),
    Ask(String),
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
}

struct App {
    mode: Mode,
    tab: Tab,
    size: Vec2,
    lean: Vec2,
    grow: f32,
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
    sheen_at: Instant,
    hover_was: bool,
    awake: bool,
    last_fg: isize,
    album: Option<[u8; 3]>,
    live: live::Live,
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
    was_foreground: bool,
    qr_text: String,
    pick_at: Option<Instant>,
    remind_text: String,
    remind_time: String,
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
    /// самообновление: состояние проверки, время следующей и момент установки
    upd: Arc<Mutex<update::State>>,
    upd_at: Instant,
    upd_go: Option<Instant>,
    /// громкость и раскладка из потока наблюдения и то, что уже показано
    watch: Arc<Mutex<sys::Watch>>,
    watch_seen: Option<sys::Watch>,
    clear_saved: u8,
    todo_edit: String,
    /// режим «Фокус»: идёт ли перерыв и сколько кругов работы пройдено
    focus: Option<(bool, u32)>,
    /// размеры файлов на полке: None — файла нет, u64::MAX — папка
    shelf_meta: std::collections::HashMap<String, Option<u64>>,
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
                *apps.lock().unwrap() = list;
            });
        }

        if !test_mode() {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            sys::spawn_downloads(move |name| {
                let _ = tx.send(Msg::Download(name));
                ctx.request_repaint();
            });
        }
        let (lens_tx, lens_rx) = glass::spawn(ctx.clone());
        let fx = cc.gl.as_ref().and_then(|gl| fx::Fx::new(gl)).map(Arc::new);
        let cfg = store::Settings::load();
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
            lean: Vec2::ZERO,
            grow: 0.0,
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
            ctl_at: now,
            mixer: Vec::new(),
            sound: sound::Sound::spawn(),
            bars: [0.0; sound::BANDS],
            beat: 0.0,
            sounding: (String::new(), now),
            mixer_at: now.checked_sub(Duration::from_secs(5)).unwrap_or(now),
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
            was_foreground: false,
            qr_text: String::new(),
            pick_at: None,
            remind_text: String::new(),
            remind_time: String::new(),
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
            procs_at: now.checked_sub(Duration::from_secs(60)).unwrap_or(now),
            upd: Default::default(),
            upd_at: now + Duration::from_secs(15),
            upd_go: None,
            watch: {
                let c = cc.egui_ctx.clone();
                sys::spawn_watch(move || c.request_repaint())
            },
            watch_seen: None,
            clear_saved: 0,
            todo_edit: String::new(),
            focus: None,
            shelf_meta: Default::default(),
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
        if test_mode() && std::env::var_os("OSTROVOK_TODO").is_some() {
            app.cfg.todos = [("Купить хлеб и молоко", false), ("Ответить Мирону", false), ("Оплатить интернет", true), ("Записаться к врачу", false)].map(|(t, done)| store::Todo { text: t.to_string(), done }).to_vec();
        }
        if test_mode() && std::env::var_os("OSTROVOK_SHELF").is_some() {
            app.cfg.shelf = ["C:\\Windows\\explorer.exe", "C:\\Windows\\System32\\notepad.exe", "C:\\Windows\\win.ini", "C:\\Windows\\Fonts", "C:\\нет\\такого.txt"].map(String::from).to_vec();
        }
        // для снимков: открыть строку поиска с готовым запросом
        if let Ok(q) = std::env::var("OSTROVOK_QUERY") {
            app.query = q;
            app.build_results();
            app.palette_demo = true;
        }
        if let Ok(t) = std::env::var("OSTROVOK_HINT") {
            app.hint = smart_hint(&t);
            app.hint_until = Instant::now() + Duration::from_secs(60);
        }
        update::cleanup();
        if !test_mode() && app.cfg.last_version != env!("CARGO_PKG_VERSION") {
            if !app.cfg.last_version.is_empty() {
                app.show_toast("\u{E930}", [0x7B, 0xE0, 0x9A], "Островок обновлён", concat!("версия ", env!("CARGO_PKG_VERSION")), 6, false);
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
                    }
                    Tab::Shot
                }
                "notes" => {
                    // для снимков: образцы заметок (в проверочном запуске они не сохраняются)
                    app.notes = ["Покупки\nхлеб, молоко, сыр\nкорм коту", "Идеи для острова\nгромкость по приложениям колесом\nвиджет курса валют", "Пароль от вайфая у соседей\nспросить в субботу", "Сервер\nперезагружать по вторникам"].map(String::from).to_vec();
                    Tab::Notes
                }
                "jarvis" => Tab::Jarvis,
                "jarvis-settings" => {
                    app.jv_view = 1;
                    Tab::Jarvis
                }
                "control" => Tab::Control,
                "music" => Tab::Music,
                "remind" => Tab::Remind,
                "qr" => Tab::Qr,
                "shelf" => Tab::Shelf,
                "weather" => Tab::Weather,
                "timer" => Tab::Timer,
                "calendar" => {
                    // для снимков: несколько дел на ближайшие дни
                    let now = chrono::Local::now().timestamp();
                    for (days, hour, text) in [(0, 3, "Созвон с командой"), (0, 5, "Забрать посылку"), (2, 4, "Оплатить интернет"), (6, 2, "День рождения Миши")] {
                        app.cfg.reminders.push(store::Reminder { text: text.into(), at: now + days * 86400 + hour * 3600 });
                    }
                    Tab::Calendar
                }
                "clip" => {
                    // для снимков: образцы записей, чтобы страница не была пустой
                    app.clips = ["https://github.com/qutlawsoasis-debug/Ostrovok", "#7C5CFF", "128 * 4 + 16", "Купить хлеб и молоко по дороге домой", r"C:\Windows\System32", "hello@example.org"].map(String::from).to_vec();
                    Tab::Clip
                }
                "todo" => Tab::Todo,
                "system" => Tab::System,
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
        if minutes <= 0.0 || minutes > 24.0 * 60.0 {
            return;
        }
        self.timer_end = Some(Instant::now() + Duration::from_secs_f64(minutes * 60.0));
        self.timer_total = (minutes * 60.0) as f32;
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
        let (tx, ctx, name) = (self.tx.clone(), ctx.clone(), self.cfg.city_name.clone());
        std::thread::spawn(move || {
            if let Ok(w) = weather::fetch(lat, lon, &name) {
                let _ = tx.send(Msg::Weather(w));
                ctx.request_repaint();
            }
        });
    }

    fn save_city(&mut self, ctx: &egui::Context) {
        let city = self.city_edit.trim().to_string();
        if city.is_empty() {
            self.cfg.city.clear();
            self.cfg.city_name.clear();
            self.cfg.lat = None;
            self.cfg.lon = None;
            self.cfg.save();
            self.weather = None;
            self.notify("Погода отключена");
            return;
        }
        self.cfg.city = city.clone();
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let msg = match weather::geocode(&city) {
                Ok(Some((lat, lon, name))) => Msg::City(lat, lon, name),
                Ok(None) => Msg::Note("Город не найден".into()),
                Err(_) => Msg::Note("Нет связи с сервером погоды".into()),
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

        for prefix in ["таймер ", "timer "] {
            if let Some(rest) = low.strip_prefix(prefix) {
                if let Ok(m) = rest.trim().replace(',', ".").parse::<f64>() {
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
            let action = if value.is_empty() { Action::Copy(String::new()) } else { Action::Copy(value) };
            self.results.push(Item { title, sub: sub.into(), action });
        }
        let converted = !self.results.is_empty();

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
            let apps = self.apps.lock().unwrap();
            let mut found: Vec<(u8, &(String, String))> = apps
                .iter()
                .filter_map(|a| {
                    let n = a.0.to_lowercase();
                    if n.starts_with(&low) {
                        Some((0, a))
                    } else if n.contains(&format!(" {low}")) {
                        Some((1, a))
                    } else if n.contains(&low) {
                        Some((2, a))
                    } else {
                        None
                    }
                })
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

    fn run(&mut self, action: Action) {
        self.mode = Mode::Pill;
        match action {
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
                Msg::Weather(w) => self.weather = Some(w),
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
                }
                Msg::Download(name) => self.show_toast("\u{E896}", [0x3A, 0x9B, 0xFF], "Загрузка завершена", &name, 6, false),
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
                Msg::City(lat, lon, name) => {
                    self.cfg.lat = Some(lat);
                    self.cfg.lon = Some(lon);
                    self.cfg.city_name = name.clone();
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
            // проверка вида пилюли без плеера: OSTROVOK_TRACK=«название»
            if let (None, Ok(t)) = (&self.media_now, std::env::var("OSTROVOK_TRACK")) {
                self.media_now = Some(media::Info { title: t, artist: "Исполнитель".into(), playing: true, pos: 64.0, dur: 200.0, at: Instant::now() });
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
            match self.upd_go {
                // во время игры или другого полноэкранного приложения остров не перезапускается:
                // новое окно может отобрать у игры фокус, и она свернётся. Обновление подождёт.
                None if self.mode == Mode::Pill && !self.hover_was && self.toast.is_none() && !self.hidden => {
                    self.show_toast("\u{E896}", [0x8F, 0xD3, 0xFF], "Обновляю Островок", &format!("ставлю версию {version}"), 3, false);
                    self.upd_go = Some(now + Duration::from_millis(1800));
                }
                Some(at) if now >= at => {
                    self.upd_go = None;
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
                        }
                    }
                }
                _ => {}
            }
        }

        let seq = sys::clipboard_seq();
        if seq != self.clip_seq {
            self.clip_seq = seq;
            if let Some(Ok(t)) = self.clipboard.as_mut().map(|cb| cb.get_text()) {
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
            self.cfg.reminders.retain(|r| r.at > stamp);
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
                self.show_toast("\u{E720}", [0x58, 0x65, 0xF2], "Звонок в Discord", "Микрофон используется", 4, false);
            }
            (false, Some(since)) => {
                self.call_since = None;
                let secs = now.duration_since(since).as_secs();
                self.show_toast("\u{E720}", [0x58, 0x65, 0xF2], "Звонок завершён", &format!("Длился {:02}:{:02}", secs / 60, secs % 60), 4, false);
            }
            _ => {}
        }

        if self.cfg.lat.is_some() && self.weather_at.map_or(true, |t| now.duration_since(t) > Duration::from_secs(900)) {
            self.refresh_weather(ctx);
        }

        if self.notes_dirty.is_some_and(|t| now.duration_since(t) > Duration::from_secs(1)) {
            store::save_notes(&self.notes);
            self.notes_dirty = None;
        }

        self.hidden = !test_mode() && sys::fullscreen_app_active();
        let fg = sys::foreground_other();
        if fg != 0 {
            self.last_fg = fg;
        }
        if now.duration_since(self.stats_at) >= Duration::from_millis(1500) {
            self.stats_at = now;
            self.stats = self.meter.sample();
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
        if let (Some(end), true) = (self.timer_end, self.fx.is_none()) {
            v.push(Seg::Text(fmt_left(end), accent()));
        }
        if let (Some(since), true) = (self.call_since, self.fx.is_none()) {
            let secs = since.elapsed().as_secs();
            v.push(Seg::Text(format!("\u{E720} {:02}:{:02}", secs / 60, secs % 60), Color32::from_rgb(0x8E, 0x98, 0xFF)));
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
            } + 10.0;
        }
        (w + 6.0).clamp(120.0, 520.0)
    }

    /// Персонаж в пилюле. Прорисовка глаз и персонажей перенесена из Atoll (Views/CharacterView.cs, MIT).
    fn draw_face(&self, p: &egui::Painter, o: Pos2, look: Vec2, now: Instant) {
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

    fn draw_pill(&self, ui: &egui::Ui, rect: Rect, segs: &[Seg], time: f64) {
        let p = ui.painter().with_clip_rect(rect);
        let cy = rect.center().y;
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
            }
        }
        // ход трека — тонкая линия по нижней кромке пилюли
        if let Some(m) = self.media_now.as_ref().filter(|m| m.playing && m.dur > 0.0) {
            let frac = ((m.pos + m.at.elapsed().as_secs_f64()).min(m.dur) / m.dur) as f32;
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
            ("\u{E70B}", "Записи", &[(Tab::Clip, "Буфер"), (Tab::Todo, "Дела"), (Tab::Notes, "Заметки"), (Tab::Shelf, "Полка")]),
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
            Tab::Calendar => self.ui_calendar(ui),
            Tab::System => self.ui_system(ui),
            Tab::Sound => self.ui_sound(ui),
            Tab::Weather => self.ui_weather(ui),
            Tab::Timer => self.ui_timer(ui),
            Tab::Clip => self.ui_clip(ui),
            Tab::Notes => self.ui_notes(ui),
            Tab::Todo => self.ui_todo(ui),
            Tab::Shot => self.ui_shot(ui),
            Tab::Settings => self.ui_settings(ui, ctx),
        }
    }

    /// Инструменты → QR-код.
    fn ui_qr(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 120.0).max(120.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.qr_text)
                    .margin(egui::Margin::symmetric(9.0, 7.0))
                    .hint_text(RichText::new("Текст или ссылка").color(DIM))
                    .desired_width(w),
            );
            if jelly(ui, "Из буфера").clicked() {
                self.qr_text = self.clipboard.as_mut().and_then(|cb| cb.get_text().ok()).unwrap_or_default().chars().take(400).collect();
            }
        });
        let text = self.qr_text.trim();
        let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), 196.0), Sense::hover());
        if text.is_empty() {
            empty_at(ui.painter(), area.center(), "\u{ED14}", "Кода пока нет", "Впишите текст или ссылку — здесь появится код");
            return;
        }
        match qrcode::QrCode::new(text.as_bytes()) {
            Ok(code) => {
                let n = code.width();
                let side = 168.0;
                let sq = Rect::from_center_size(area.center(), vec2(side, side));
                let p = ui.painter();
                p.rect_filled(sq.expand(10.0), Rounding::same(12.0), Color32::WHITE);
                let cell = side / n as f32;
                for (i, c) in code.to_colors().iter().enumerate() {
                    if *c == qrcode::Color::Dark {
                        let at = sq.min + vec2((i % n) as f32 * cell, (i / n) as f32 * cell);
                        p.rect_filled(Rect::from_min_size(at, vec2(cell + 0.35, cell + 0.35)), Rounding::ZERO, Color32::BLACK);
                    }
                }
            }
            Err(_) => {
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
        ui.label(RichText::new("Время — часы и минуты (18:30) или через сколько минут (+15).").color(DIM).size(11.5));
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
                p.text(pos2(row.left() + 120.0, row.center().y), Align2::LEFT_CENTER, &r.text, FontId::proportional(13.5), FG);
                let x = Rect::from_center_size(pos2(row.right() - 20.0, row.center().y), vec2(26.0, 26.0));
                let resp = ui.interact(x, ui.id().with(("del-reminder", i)), Sense::click());
                ui.painter().text(x.center(), Align2::CENTER_CENTER, "\u{E711}", FontId::proportional(12.0), if resp.hovered() { FG } else { DIM });
                if resp.clicked() {
                    remove = Some(i);
                }
            }
        });
        if let Some(i) = remove {
            self.cfg.reminders.remove(i);
            self.cfg.save();
        }
    }

    fn add_reminder(&mut self) {
        use chrono::{Local, NaiveTime, TimeZone};
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
            Local.from_local_datetime(&day).single().map(|d| d.timestamp())
        } else {
            spec.trim_start_matches('+').parse::<f64>().ok().filter(|m| *m > 0.0 && *m < 60.0 * 24.0 * 30.0).map(|m| now.timestamp() + (m * 60.0) as i64)
        };
        match at {
            Some(at) => {
                self.cfg.reminders.push(store::Reminder { text, at });
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
                    .hint_text(RichText::new("Что нужно сделать").color(DIM))
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
                self.cfg.todos.insert(0, store::Todo { text, done: false });
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
        ui.horizontal(|ui| {
            ui.label(RichText::new(if left == 0 { "Всё сделано".to_string() } else { format!("Осталось: {left}") }).color(DIM));
            if done > 0 {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    clear_done = jelly(ui, "Убрать сделанные").clicked();
                });
            }
        });
        let (mut toggle, mut remove) = (None, None);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            // сначала несделанные, потом сделанные; внутри — в порядке добавления
            let order: Vec<usize> = (0..self.cfg.todos.len()).filter(|i| !self.cfg.todos[*i].done).chain((0..self.cfg.todos.len()).filter(|i| self.cfg.todos[*i].done)).collect();
            for i in order {
                let item = &self.cfg.todos[i];
                let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width() - 6.0, 34.0), Sense::click());
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.10);
                let k = ui.ctx().animate_bool_with_time(ui.id().with(("todo-done", &item.text)), item.done, 0.16);
                let p = ui.painter();
                if hov > 0.0 {
                    p.rect_filled(row, Rounding::same(10.0), rgba(255, 255, 255, (14.0 * hov) as u8));
                }
                let c = pos2(row.left() + 18.0, row.center().y);
                let a = accent();
                p.circle_stroke(c, 8.5, Stroke::new(1.5_f32, mix(DIM, a, k.max(hov * 0.6))));
                if k > 0.0 {
                    p.circle_filled(c, 8.5 * k, a);
                    p.text(c, Align2::CENTER_CENTER, "\u{E73E}", FontId::proportional(9.5), Color32::from_rgba_unmultiplied(16, 14, 28, (k * 255.0) as u8));
                }
                let text = p
                    .with_clip_rect(Rect::from_min_max(pos2(row.left() + 36.0, row.top()), pos2(row.right() - 34.0, row.bottom())))
                    .text(pos2(row.left() + 36.0, row.center().y), Align2::LEFT_CENTER, &item.text, FontId::proportional(13.5), mix(FG, DIM, k));
                if k > 0.0 {
                    let end = text.left() + text.width().min(row.right() - 34.0 - text.left()) * k;
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
            ui.label(RichText::new("Щелчок открывает файл, правый щелчок — его папку.").color(DIM));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if jelly(ui, "Очистить").clicked() {
                    self.cfg.shelf.clear();
                    self.cfg.save();
                }
            });
        });
        let (mut open, mut remove) = (None, None);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                for (i, path) in self.cfg.shelf.iter().enumerate() {
                    let (cell, resp) = ui.allocate_exact_size(vec2(104.0, 96.0), Sense::click());
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
                        sys::icon_rgba(path).map(|(w, h, px)| ui.ctx().load_texture(format!("shelf-{i}"), egui::ColorImage::from_rgba_unmultiplied([w, h], &px), egui::TextureOptions::LINEAR))
                    });
                    let side = 36.0 + 3.0 * hov - 3.0 * down;
                    let icon_r = Rect::from_center_size(pos2(cell.center().x, cell.top() + 30.0), vec2(side, side));
                    match icon {
                        Some(tex) if size.is_some() => rounded_image(ui.painter(), tex, icon_r, 0.0),
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
        if let Some(path) = open {
            sys::open(&path);
        }
        if let Some(i) = remove {
            let path = self.cfg.shelf.remove(i);
            self.shelf_meta.remove(&path);
            self.cfg.save();
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
            None => {
                p.rect_filled(art, Rounding::same(14.0), tint(20));
                p.text(art.center(), Align2::CENTER_CENTER, "\u{E8D6}", FontId::proportional(24.0), DIM);
            }
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
                    let done = Rect::from_min_max(track.min, pos2(track.left() + track.width() * (at.min(m.dur) / m.dur) as f32, track.bottom()));
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
                let clip = p.with_clip_rect(weather_r.shrink2(vec2(10.0, 0.0)));
                clip.text(weather_r.min + vec2(16.0, 8.0), Align2::LEFT_TOP, format!("{icon} {}", weather::deg(w.temp)), light(30.0), FG);
                clip.text(weather_r.min + vec2(17.0, 54.0), Align2::LEFT_TOP, text, FontId::proportional(12.5), FG);
                clip.text(weather_r.min + vec2(17.0, 75.0), Align2::LEFT_TOP, &w.city, FontId::proportional(11.5), DIM);
            }
            None => {
                p.text(weather_r.min + vec2(16.0, 30.0), Align2::LEFT_TOP, "Погода", semi(14.0), FG);
                p.text(weather_r.min + vec2(16.0, 51.0), Align2::LEFT_TOP, "укажите город", FontId::proportional(12.0), DIM);
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
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 60.0), Sense::hover());
        let step = row.width() / ACTIONS.len() as f32;
        for (i, (id, glyph, label, col)) in ACTIONS.iter().enumerate() {
            let r = Rect::from_min_size(pos2(row.left() + step * i as f32, row.top()), vec2(step, row.height()));
            let resp = ui.interact(r, ui.id().with(("act", *id)), Sense::click()).on_hover_text(match *id {
                "region" => "Выделите часть экрана — снимок попадёт в буфер",
                "full" => "Снимок всего экрана: файл в «Изображения\\Островок» и копия в буфере",
                "ocr" => "Выделите область — текст с неё скопируется",
                "qr" => "QR-код из текста или ссылки в буфере",
                "picker" => "Цвет точки под курсором, через 3 секунды",
                "cleantext" => "Убрать оформление у текста в буфере",
                "remind" => "Добавить напоминание",
                _ => "Строка поиска и команд",
            });
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
                    let apps = self.apps.lock().unwrap();
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
            None => {
                p.rect_filled(art, Rounding::same(20.0), tint(22));
                p.text(art.center(), Align2::CENTER_CENTER, "\u{E8D6}", FontId::proportional(34.0), DIM);
            }
        }
        let x = art.right() + 18.0;
        let text = p.with_clip_rect(Rect::from_min_max(pos2(x, row.top()), row.max));
        text.text(pos2(x, row.top() + 4.0), Align2::LEFT_TOP, &m.title, semi(18.0), FG);
        text.text(pos2(x, row.top() + 30.0), Align2::LEFT_TOP, &m.artist, FontId::proportional(13.5), DIM);
        // полоса прогресса со временем
        let track = Rect::from_min_max(pos2(x, row.top() + 62.0), pos2(row.right(), row.top() + 67.0));
        p.rect_filled(track, Rounding::same(2.5), rgba(255, 255, 255, 0x33));
        if m.dur > 0.0 {
            let done = Rect::from_min_max(track.min, pos2(track.left() + track.width() * (at / m.dur) as f32, track.bottom()));
            p.rect_filled(done, Rounding::same(2.5), accent);
            p.circle_filled(pos2(done.right(), track.center().y), 5.0, FG);
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
        spectrum(&p.with_clip_rect(area), area.shrink2(vec2(14.0, 0.0)), &self.bars, accent, 0.26, 0.62);
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
            lines.text(area.center(), Align2::CENTER_CENTER, "Для этого трека текста с отметками времени не нашлось", FontId::proportional(13.0), DIM);
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
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Город не указан").font(semi(16.5)));
                ui.label(RichText::new("Погода появится, когда вы впишете город в настройках.").color(DIM));
                ui.add_space(6.0);
                if jelly(ui, "Открыть настройки").clicked() {
                    self.tab = Tab::Settings;
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

        // ближайшие сутки: плавная кривая температуры, под ней — вероятность осадков
        if w.hours.len() >= 4 {
            let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), 132.0), Sense::hover());
            let p = ui.painter().with_clip_rect(area);
            glass_chip(&p, area, 16.0, 0.4);
            let n = w.hours.len();
            let (lo, hi) = w.hours.iter().fold((f64::MAX, f64::MIN), |(lo, hi), h| (lo.min(h.temp), hi.max(h.temp)));
            let span = (hi - lo).max(2.0);
            let (top, bottom) = (area.top() + 34.0, area.bottom() - 40.0);
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
            p.add(egui::Shape::line(line, Stroke::new(2.2, c)));
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
            let wet = w.hours.iter().map(|h| h.rain).fold(0.0, f64::max);
            if wet >= 15.0 {
                p.text(area.right_top() + vec2(-12.0, 8.0), Align2::RIGHT_TOP, format!("осадки до {wet:.0}%"), FontId::proportional(11.0), Color32::from_rgb(0x8F, 0xCB, 0xFF));
            }
            ui.add_space(6.0);
        }

        if !w.days.is_empty() {
            ui.columns(w.days.len(), |c| {
                for (i, d) in w.days.iter().enumerate() {
                    let (r, _) = c[i].allocate_exact_size(vec2(c[i].available_width(), 74.0), Sense::hover());
                    let p = c[i].painter();
                    glass_chip(p, r, 12.0, if i == 0 { 0.75 } else { 0.4 });
                    p.text(r.center_top() + vec2(0.0, 8.0), Align2::CENTER_TOP, if i == 0 { "Сегодня" } else { d.label.as_str() }, FontId::proportional(12.0), DIM);
                    p.text(r.center() + vec2(0.0, -1.0), Align2::CENTER_CENTER, weather::describe(d.code).0, FontId::proportional(19.0), FG);
                    p.text(r.center_bottom() - vec2(0.0, 8.0), Align2::CENTER_BOTTOM, format!("{} / {}", weather::deg(d.max), weather::deg(d.min)), semi(12.5), FG);
                }
            });
        }
    }

    /// Инструменты → Таймер: циферблат с остатком, готовые отрезки, своё время и секундомер.
    fn ui_timer(&mut self, ui: &mut egui::Ui) {
        let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), 156.0), Sense::hover());
        let p = ui.painter().clone();
        let c = pos2(area.left() + 80.0, area.center().y);
        let (text, frac) = match self.timer_end {
            Some(end) => (fmt_left(end), end.saturating_duration_since(Instant::now()).as_secs_f32() / self.timer_total.max(1.0)),
            None => ("00:00".to_string(), 0.0),
        };
        let live = self.timer_end.is_some();
        ring(&p, c, 66.0, 7.0, frac, Color32::from_rgb(0xFF, 0xB3, 0x40));
        p.text(c - vec2(0.0, 5.0), Align2::CENTER_CENTER, text, light(30.0), if live { FG } else { DIM });
        p.text(c + vec2(0.0, 24.0), Align2::CENTER_CENTER, match (live, self.focus) {
                (true, Some((true, _))) => "перерыв",
                (true, Some((false, _))) => "фокус",
                (true, None) => "осталось",
                _ => "таймер",
            }, FontId::proportional(11.5), DIM);

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
                self.timer_end = None;
                self.focus = None;
            }
            let label = match self.focus {
                Some((_, rounds)) => format!("Фокус: круг {}", rounds + 1),
                None => "Фокус 25/5".to_string(),
            };
            if jelly(ui, label).on_hover_text("25 минут работы, 5 минут перерыва — по кругу, пока не остановите").clicked() {
                if self.focus.take().is_some() {
                    self.timer_end = None;
                } else {
                    self.focus = Some((false, 0));
                    self.start_timer(25.0);
                }
            }
        });
        if let Some(m) = start {
            self.focus = None;
            self.start_timer(m);
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

    /// Записи → Буфер: история скопированного; для ссылок, цветов, примеров и путей — готовое действие.
    fn ui_clip(&mut self, ui: &mut egui::Ui) {
        if self.clips.is_empty() && self.cfg.pins.is_empty() {
            empty_state(ui, "\u{E77F}", "Буфер пуст", "Скопируйте текст — он появится здесь, а остров подскажет, что с ним сделать");
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
            if jelly(ui, "Очистить").on_hover_text("Убрать историю. Закреплённые записи останутся").clicked() {
                self.clips.clear();
            }
        });
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

    /// Показывает карточку снимка: миниатюра и что с ним можно сделать.
    fn show_shot(&mut self, ctx: &egui::Context, w: usize, h: usize, px: Vec<u8>, path: String) {
        // миниатюра — не больше 640 точек по длинной стороне
        let k = (640.0 / w.max(h) as f32).min(1.0);
        let (tw, th) = (((w as f32 * k) as u32).max(1), ((h as f32 * k) as u32).max(1));
        let Some(img) = image::RgbaImage::from_raw(w as u32, h as u32, px.clone()) else { return };
        let small = image::imageops::thumbnail(&img, tw, th);
        let tex = ctx.load_texture("shot-card", egui::ColorImage::from_rgba_unmultiplied([tw as usize, th as usize], small.as_raw()), Default::default());
        self.shot_card = Some(ShotCard { path, tex, size: (w, h), rgba: Arc::new(px), until: Instant::now() + Duration::from_secs(8) });
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

        let x = frame.right() + 18.0;
        p.text(pos2(x, row.top() + 4.0), Align2::LEFT_TOP, "Снимок сохранён", semi(17.0), FG);
        p.with_clip_rect(Rect::from_min_max(pos2(x, row.top()), row.max))
            .text(pos2(x, row.top() + 30.0), Align2::LEFT_TOP, format!("{} × {} · уже в буфере обмена", size.0, size.1), FontId::proportional(12.5), DIM);
        p.with_clip_rect(Rect::from_min_max(pos2(x, row.top()), row.max))
            .text(pos2(x, row.top() + 48.0), Align2::LEFT_TOP, &name, FontId::proportional(12.0), mix(DIM, BG, 0.25));
        let mut act = None;
        let mut side = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(pos2(x, row.top() + 74.0), row.max)));
        side.horizontal_wrapped(|ui| {
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
                let more = lines.next().unwrap_or("").to_string();
                let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.1);
                let on = i == self.note_sel;
                glass_chip(ui.painter(), r, 11.0, if on { 1.0 } else { 0.3 + 0.4 * hov });
                let p = ui.painter().with_clip_rect(r.shrink2(vec2(10.0, 0.0)));
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
        egui::ScrollArea::vertical().id_salt("notes-text").max_height(right.height() - 30.0).auto_shrink([false, false]).show(&mut ru, |ui| {
            let r = ui.add(
                egui::TextEdit::multiline(&mut self.notes[sel])
                    .hint_text(RichText::new("Первая строка станет названием. Заметки сохраняются сами").color(DIM))
                    .desired_width(f32::INFINITY)
                    .desired_rows(11)
                    .margin(egui::Margin::symmetric(10.0, 8.0)),
            );
            if r.changed() {
                self.notes_dirty = Some(Instant::now());
            }
        });
        ru.horizontal(|ui| {
            let n = self.notes[sel].chars().count();
            ui.label(RichText::new(format!("{} из {} · {n} симв.", sel + 1, self.notes.len())).color(DIM).size(11.5));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                del = ui.add_enabled(self.notes.len() > 1 || n > 0, egui::Button::new("Удалить")).clicked();
                if jelly(ui, "Копировать").clicked() {
                    let text = self.notes[sel].clone();
                    self.set_clipboard(&text);
                    self.notify("Заметка скопирована");
                }
            });
        });

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
                for (i, t) in THEMES.iter().enumerate() {
                    let (r, resp) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::click());
                    let resp = resp.on_hover_text(t.name);
                    let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.1);
                    let sel = ui.ctx().animate_bool_with_time(resp.id.with("sel"), i == cur, 0.14);
                    let col = Color32::from_rgb(t.accent[0], t.accent[1], t.accent[2]);
                    // кружок показывает и акцент, и оттенок стекла
                    ui.painter().circle_filled(r.center(), 11.0 + hov, Color32::from_rgb(t.glass[0] * 3, t.glass[1] * 3, t.glass[2] * 3));
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
                ui.add_space(4.0);
                ui.label(RichText::new(theme().name).font(semi(13.0)).color(accent()));
            });
            ui.add_space(4.0);
            head(ui, "ПЕРСОНАЖ");
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 5.0;
                for (i, name) in ["Глазки", "Робот", "Кот", "Призрак", "Слайм"].into_iter().enumerate() {
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
                ui.label(RichText::new(format!("версия {}", env!("CARGO_PKG_VERSION"))).color(DIM));
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
            ui.label(RichText::new(format!("Поиск открывается по {}. Щелчок по персонажу меняет его настроение.", self.hotkey_name)).color(DIM).size(11.5));
        });
    }

    /// Прозрачность стекла в процентах (в настройках 0 значит «не трогали»).
    fn clearness(&self) -> u8 {
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
        use chrono::{Datelike, Local, NaiveDate, NaiveTime, TimeZone};
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
            for (i, name) in ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"].into_iter().enumerate() {
                p.text(origin + vec2(cell.x * (i as f32 + 0.5), 10.0), Align2::CENTER_CENTER, name, FontId::proportional(11.5), DIM);
            }
            for day in 1..=days {
                let Some(date) = NaiveDate::from_ymd_opt(year, month0 + 1, day) else { continue };
                let slot = offset + day - 1;
                let (col, row) = (slot % 7, slot / 7);
                let at = origin + vec2(cell.x * (col as f32 + 0.5), 24.0 + cell.y * (row as f32 + 0.5));
                let resp = ui.interact(Rect::from_center_size(at, vec2(cell.x.min(40.0), 29.0)), ui.id().with(("cal-day", year, month0, day)), Sense::click());
                let hov = ui.ctx().animate_bool_with_time(resp.id.with("hov"), resp.hovered(), 0.1);
                let (is_today, is_sel) = (date == today, date == sel);
                if is_today {
                    p.circle_filled(at, 13.0, accent());
                } else if is_sel {
                    p.circle_filled(at, 13.0, rgba(255, 255, 255, 26));
                    p.circle_stroke(at, 13.0, Stroke::new(1.5, accent()));
                } else if hov > 0.0 {
                    p.circle_filled(at, 13.0, rgba(255, 255, 255, (26.0 * hov) as u8));
                }
                if is_today && is_sel && self.cal_sel.is_some() {
                    p.circle_stroke(at, 15.5, Stroke::new(1.2, accent()));
                }
                let color = if is_today { BG } else if date < today { mix(DIM, BG, 0.35) } else if col >= 5 { DIM } else { FG };
                p.text(at, Align2::CENTER_CENTER, day.to_string(), semi(13.5), color);
                // точка — на этот день есть дела
                if dated.iter().any(|d| d.1 == date) {
                    p.circle_filled(at + vec2(0.0, if is_today { 9.0 } else { 11.0 }), 2.0, if is_today { BG } else { accent() });
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
            ui.add_space(6.0);
            let mine: Vec<&(usize, NaiveDate, String, String)> = dated.iter().filter(|d| d.1 == sel).collect();
            egui::ScrollArea::vertical().max_height(118.0).auto_shrink([false, true]).show(ui, |ui| {
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
            let at = time.and_then(|t| Local.from_local_datetime(&sel.and_time(t)).single()).map(|d| d.timestamp());
            match at {
                _ if text.is_empty() => self.notify("Впишите, о чём напомнить"),
                None => self.notify("Не понял время: напишите, например, 18:30"),
                Some(at) if at <= Local::now().timestamp() => self.notify("Это время уже прошло"),
                Some(at) => {
                    self.cfg.reminders.push(store::Reminder { text, at });
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
            glass_chip(ui.painter(), r, 18.0, 0.55);
            let shown = ui.ctx().animate_value_with_time(ui.id().with(("sys", k)), frac.clamp(0.0, 1.0), 0.5);
            let c = pos2(r.center().x, r.top() + 46.0);
            ring(ui.painter(), c, 30.0, 6.0, shown, if alarm && shown > 0.9 { Color32::from_rgb(0xFF, 0x6B, 0x6B) } else { color });
            ui.painter().text(c, Align2::CENTER_CENTER, value, semi(14.0), FG);
            ui.painter().text(pos2(c.x, r.top() + 96.0), Align2::CENTER_CENTER, name, semi(12.5), FG);
            ui.painter().text(pos2(c.x, r.top() + 114.0), Align2::CENTER_CENTER, detail, FontId::proportional(11.5), DIM);
        }
        if s.battery.is_some() {
            ui.label(RichText::new(format!("Компьютер работает {d} д {h} ч {m} мин без перезагрузки")).color(DIM));
        }
        ui.add_space(4.0);

        // список программ обновляется раз в три секунды в отдельном потоке, чтобы панель не подтормаживала
        if self.procs_at.elapsed() > Duration::from_secs(3) {
            self.procs_at = Instant::now();
            let (procs, ctx) = (self.procs.clone(), ui.ctx().clone());
            std::thread::spawn(move || {
                let list = sys::top_memory(5);
                *procs.lock().unwrap() = list;
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
            p.line_segment([pos2(plot.left(), y), pos2(plot.right(), y)], Stroke::new(1.0, rgba(255, 255, 255, 14)));
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
                        pos2(plot.right() - plot.width() * (n - 1 - i) as f32 / 119.0, plot.bottom() - plot.height() * val.clamp(0.0, 1.0))
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
                p.add(egui::Shape::line(pts, Stroke::new(1.8, color)));
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
        p.text(right.min + vec2(14.0, 10.0), Align2::LEFT_TOP, "Больше всего памяти", semi(12.5), FG);
        let list = self.procs.lock().unwrap().clone();
        let top = list.first().map_or(1, |l| l.1).max(1) as f32;
        let total = (s.mem_total * 1024.0 * 1024.0 * 1024.0).max(1.0);
        let mut kill: Option<String> = None;
        for (i, (name, bytes)) in list.iter().enumerate() {
            let y = right.top() + 40.0 + i as f32 * 20.5;
            let size = if *bytes >= 1 << 30 { format!("{:.1} ГБ", *bytes as f64 / (1u64 << 30) as f64).replace('.', ",") } else { format!("{} МБ", bytes >> 20) };
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
            // красным — программа занимает больше четверти всей памяти
            let heavy = *bytes as f64 / total > 0.25;
            let fill = Rect::from_min_max(track.min, pos2(track.left() + track.width() * (*bytes as f32 / top).clamp(0.03, 1.0), track.bottom()));
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
            p.text(right.center(), Align2::CENTER_CENTER, "Считаю…", FontId::proportional(12.0), DIM);
        }
    }

    fn ui_palette(&mut self, ui: &mut egui::Ui) {
        let te = ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .hint_text(RichText::new("Программа, сайт, пример, «5 км в мили», «100 usd в рубли» или «таймер 5»").color(DIM))
                .desired_width(f32::INFINITY)
                .frame(false)
                .font(FontId::proportional(17.0)),
        );
        if self.focus_query {
            te.request_focus();
            self.focus_query = false;
        }
        if te.changed() {
            self.build_results();
            self.sel = 0;
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
        for (i, it) in self.results.iter().enumerate() {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H - 2.0), Sense::click());
            if resp.hovered() || i == self.sel {
                let a = if i == self.sel { 34 } else { 16 };
                ui.painter().rect_filled(r, Rounding::same(9.0), tint(a));
            }
            let p = ui.painter().with_clip_rect(r);
            p.text(pos2(r.right() - 12.0, r.center().y), Align2::RIGHT_CENTER, &it.sub, FontId::proportional(11.5), DIM);
            p.text(pos2(r.left() + 12.0, r.center().y), Align2::LEFT_CENTER, &it.title, FontId::proportional(14.0), FG);
            if resp.clicked() {
                run = Some(i);
            }
        }
        if let Some(i) = run {
            let action = self.results[i].action.clone();
            self.run(action);
        }
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        store::save_notes(&self.notes);
        self.jarvis.stop();
        self.ears.stop_server();
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let now = Instant::now();
        let dt = ctx.input(|i| i.stable_dt).clamp(0.001, 0.05);
        let time = ctx.input(|i| i.time);
        self.pump(ctx, now);
        self.web_slot = None;
        let dropped: Vec<String> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.as_ref().map(|p| p.display().to_string())).collect());
        if !dropped.is_empty() {
            for path in dropped {
                self.cfg.shelf.retain(|p| p != &path);
                self.cfg.shelf.insert(0, path);
            }
            self.cfg.shelf.truncate(30);
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
                        vec2(if t.level.is_some() { 236.0 } else { (62.0 + w(&t.title, semi(13.5))).max(120.0) }, 40.0)
                    } else {
                        vec2((66.0 + w(&t.title, semi(13.5)).max(w(&t.text, FontId::proportional(12.0))) + 24.0).clamp(220.0, 470.0), 54.0)
                    }
                }
                None => vec2(self.pill_width(ctx, &segs), PILL_H),
            },
            Mode::Panel => vec2(PANEL_W, self.panel_height()),
            Mode::Palette => {
                let n = self.results.len() as f32;
                vec2(PANEL_W, 50.0 + n * ROW_H + if n > 0.0 { 10.0 } else { 0.0 })
            }
        };
        // пружина: остров слегка «переливается» за цель и возвращается, как капля
        let sdt = dt.min(0.02);
        self.vel += ((target - self.size) * 340.0 - self.vel * 31.0) * sdt;
        self.size += self.vel * sdt;
        self.size = self.size.max(vec2(40.0, 20.0));
        let animating = (target - self.size).length() > 0.4 || self.vel.length() > 3.0;

        let rect_v = Rect::from_min_size(pos2(WIN_W / 2.0 - self.size.x / 2.0, TOP), self.size);
        let layout_v = Rect::from_min_size(pos2(WIN_W / 2.0 - target.x / 2.0, TOP), target);

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
        let side = self.fx.is_some() && self.mode == Mode::Pill && self.toast.is_none() && !self.hidden;
        self.bub_timer.target = if side && self.timer_end.is_some() { 1.0 } else { 0.0 };
        self.bub_call.target = if side && self.call_since.is_some() { 1.0 } else { 0.0 };
        self.bub_timer.step(dt);
        self.bub_call.step(dt);
        self.bub_clip.target = if side && self.hint.is_some() && now < self.hint_until { 1.0 } else { 0.0 };
        self.bub_clip.step(dt);
        self.ripple *= (-dt * 2.3).exp();
        if self.ripple < 0.03 {
            self.ripple = 0.0;
        }
        self.ripple_phase += dt * 7.5;
        self.drip_phase += dt * 2.6;
        // остров-колба: жидкость внутри пилюли. Уровень что-то значит, поверхность живая.
        let flask = self.flask_on();
        let mut sloshing = false;
        if flask {
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
        let moving = !self.reach.settled() || self.reach.v > 0.01 || !self.lean_s.settled() || !self.jelly.settled() || self.ripple > 0.0 || dripping || glowing || glowing_music || self.sound.active() || sloshing || !self.bub_timer.settled() || !self.bub_call.settled() || !self.bub_clip.settled() || self.sheen_at.elapsed().as_secs_f32() < 0.75;
        let animating = animating || moving;
        let scaled = rect_v.size() * self.jelly.v;
        let rect_v = Rect::from_min_size(pos2(WIN_W / 2.0 - scaled.x / 2.0 + self.lean_s.v, TOP), scaled);

        // капли справа от пилюли: выезжают из неё и втягиваются обратно, по пути срастаясь «шейкой»
        let mut bubbles_v: Vec<(Rect, f32, u8, String)> = Vec::new();
        {
            let clock = |secs: u64| format!("{:02}:{:02}", secs / 60, secs % 60);
            let timer_text = self.timer_end.map(fmt_left).unwrap_or_else(|| "00:00".into());
            let call_text = clock(self.call_since.map_or(0, |s| s.elapsed().as_secs()));
            let mut edge = rect_v.right();
            let hint_text = self.hint.as_ref().map(|h| h.label.clone()).unwrap_or_default();
            for (kind, sv, text) in [(0u8, self.bub_timer.v, timer_text), (1u8, self.bub_call.v, call_text), (2u8, self.bub_clip.v, hint_text)] {
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
                                    let track = Rect::from_min_max(pos2(rect.left() + 46.0, cy - 3.0), pos2(rect.right() - 54.0, cy + 3.0));
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
                    let (mut open_timer, mut run_hint) = (false, false);
                    for (i, (r, sv, kind, text)) in self.bubbles.iter().enumerate() {
                        let a = ((sv - 0.55) / 0.45).clamp(0.0, 1.0);
                        if a <= 0.0 {
                            continue;
                        }
                        let (glyph, color) = match (*kind, &self.hint) {
                            (0, _) => ("\u{E916}", Color32::from_rgb(0xFF, 0xB3, 0x40)),
                            (2, Some(h)) => (h.glyph, Color32::from_rgb(h.color[0], h.color[1], h.color[2])),
                            _ => ("\u{E720}", Color32::from_rgb(0x9A, 0xA3, 0xFF)),
                        };
                        let fade = |c: Color32| rgba(c.r(), c.g(), c.b(), (a * 255.0) as u8);
                        ui.painter().text(pos2(r.left() + 18.0, r.center().y), Align2::CENTER_CENTER, glyph, FontId::proportional(14.0), fade(color));
                        ui.painter().text(pos2(r.left() + 31.0, r.center().y), Align2::LEFT_CENTER, text, semi(13.0), fade(FG));
                        if *kind != 1 && ui.interact(*r, Id::new(("bubble", i)), Sense::click()).clicked() {
                            if *kind == 0 {
                                open_timer = true;
                            } else {
                                run_hint = true;
                            }
                        }
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
        if animating || self.shot.is_some() {
            ctx.request_repaint();
        } else if self.mode != Mode::Pill || dist < 420.0 || playing {
            ctx.request_repaint_after(Duration::from_millis(16));
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
        let f = ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0) * (n as f32 - 1.0);
        let i = f as usize;
        let wave = self.liq_h[i] + (self.liq_h[(i + 1).min(n - 1)] - self.liq_h[i]) * (f - i as f32);
        let ys = cy + rin - self.level * 2.0 * rin + wave;
        (ys.clamp(top_b, bot_b), bot_b, ys > top_b + 0.3 && ys < bot_b - 0.3)
    }

    fn panel_height(&self) -> f32 {
        // навигация теперь в одну строку
        let saved = if matches!(self.tab, Tab::Ai | Tab::Settings) { 0.0 } else { 24.0 };
        -saved + match self.tab {
            Tab::Home => 370.0,
            Tab::Control => 238.0,
            Tab::Sound => 286.0,
            Tab::Music => 388.0,
            Tab::Remind => 320.0,
            Tab::Qr => 350.0,
            Tab::Shelf => 340.0,
            Tab::Ai if self.api_key().is_empty() => AI_H,
            Tab::Ai => 420.0,
            Tab::Jarvis => 462.0,
            Tab::Weather if self.weather.as_ref().is_some_and(|w| w.hours.len() >= 4) => 398.0,
            Tab::Weather if self.weather.is_some() => 292.0,
            Tab::Weather => 250.0,
            Tab::Timer => 262.0,
            Tab::Calendar => 360.0,
            Tab::Shot => 306.0,
            Tab::Clip => 340.0,
            Tab::Notes => 340.0,
            Tab::Todo => 340.0,
            Tab::System => if self.stats.battery.is_some() { 416.0 } else { 394.0 },
            Tab::Settings => 436.0,
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
        if let Some(p) = resp.interact_pointer_pos() {
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
    None
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

fn rounded_image(p: &egui::Painter, tex: &egui::TextureHandle, rect: Rect, radius: f32) {
    let mut s = egui::epaint::RectShape::filled(rect, Rounding::same(radius), Color32::WHITE);
    s.fill_texture_id = tex.id();
    s.uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    p.add(s);
}

fn fmt_left(end: Instant) -> String {
    let s = end.saturating_duration_since(Instant::now()).as_secs_f64().ceil() as u64;
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
    let updated = std::env::args().any(|a| a == "--updated");
    if !test && !(if updated { sys::wait_single_instance() } else { sys::single_instance() }) {
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
