//! «Игра»: пинг до роутера и интернета, что мешает играть, игровые сессии. Потоки только пишут в State, страница читает.

use std::collections::{HashMap, VecDeque};
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HANDLE;

/// замеров на странице (раз в секунду)
pub const RING: usize = 60;
/// куда пингуем интернет: если первый адрес ни разу не ответил, переходим на второй
pub const INET_TARGETS: [Ipv4Addr; 2] = [Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)];
pub const GW_TIMEOUT_MS: u32 = 500;
pub const INET_TIMEOUT_MS: u32 = 900;
/// как часто заново искать шлюз (сеть могла смениться)
pub const GW_REFRESH: Duration = Duration::from_secs(30);
/// столько можно быть вне игры (Alt+Tab), не обрывая сессию
pub const AWAY_SECS: u64 = 120;
/// короче — не игра (видео, случайное окно): без сводки
pub const MIN_GAME_SECS: u64 = 180;
/// замеров за сессию не больше четырёх часов: старое выталкивается
pub const MAX_SESSION_SAMPLES: usize = 4 * 3600;
pub const SCAN_VISIBLE: Duration = Duration::from_secs(5);
pub const SCAN_GAME: Duration = Duration::from_secs(20);
/// пороги ввода-вывода, байт/с
pub const ONEDRIVE_IO: f64 = 300_000.0;
pub const STEAM_IO: f64 = 1_000_000.0;
pub const LAUNCHER_IO: f64 = 1_000_000.0;
pub const LOW_DISK_GB: f64 = 20.0;
/// скачок — хотя бы на столько мс выше обычного пинга
pub const SPIKE_MIN_MS: u32 = 50;

/// Границы «зелёного» и «жёлтого»: (≤ зелёный, ≤ жёлтый), выше — красный.
pub struct Limits {
    pub ping: (u32, u32),
    pub jitter: (f32, f32),
    pub loss: (f32, f32),
}

/// ориентир CS2: пинг ≤60 зелёный, ≤100 жёлтый; джиттер ≤5/≤15; потери ≤0,5%/≤2%.
/// Зелёные потери не нулевые: за несколько часов игры пинг до 1.1.1.1 почти всегда теряется раз-другой —
/// одна потеря из тысяч не делает игру «терпимой». На странице (60 замеров) одна потеря — уже 1,7%, жёлтый.
pub const INET: Limits = Limits { ping: (60, 100), jitter: (5.0, 15.0), loss: (0.5, 2.0) };
/// до роутера всё должно быть почти мгновенно (Wi‑Fi 2–10 мс)
pub const GATEWAY: Limits = Limits { ping: (10, 30), jitter: (4.0, 12.0), loss: (0.5, 2.0) };

/// Единственные адреса, которые открывают кнопки страницы (кроме проверенного пути программы).
pub const ALLOWED_URIS: [&str; 5] = ["ms-settings:windowsupdate", "steam://open/downloads", "ms-settings:gaming-gamedvr", "taskmgr", "ms-settings:storagesense"];

// раскладка страницы (её же использует ui_game)
pub const HERO_H: f32 = 90.0;
pub const GRAPH_H: f32 = 104.0;
pub const LIST_HEAD_H: f32 = 18.0;
pub const ROW_H: f32 = 38.0;
pub const EMPTY_ROW_H: f32 = 30.0;
pub const LAST_H: f32 = 60.0;
pub const LAST_EMPTY_H: f32 = 44.0;
pub const MAX_ROWS: usize = 4;
/// item_spacing.y страницы: добавляется после каждого allocate
const GAP: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Green,
    Yellow,
    Red,
}

/// Где сеть плохая: дома (до роутера), дальше (провайдер, забитый канал) или не понять.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Fine,
    Home,
    Beyond,
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub sent: usize,
    pub lost: usize,
    pub avg: f32,
    pub min: u32,
    pub max: u32,
    pub median: u32,
    pub jitter: f32,
}

/// Что может мешать игре; порядок = приоритет (выше — вероятнее мешает).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Updates,
    Torrent,
    Steam,
    Launcher,
    OneDrive,
    DiscordClips,
    NvidiaReplay,
    XboxDvr,
    Build,
}

/// Куда ведёт кнопка: постоянный адрес из ALLOWED_URIS или путь найденной программы.
#[derive(Clone, Debug, PartialEq)]
pub enum Open {
    Uri(&'static str),
    Exe(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Culprit {
    pub kind: Kind,
    pub title: String,
    pub detail: String,
    pub open: Option<Open>,
    /// мешает прямо сейчас (идёт установка, видна скорость), а не только косвенно
    /// (служба запущена, флаг в реестре) — только такие помехи можно винить в плохой сети
    pub sure: bool,
}

/// Запущенный процесс из тех, за которыми следим.
#[derive(Clone, Debug, PartialEq)]
pub struct Proc {
    pub pid: u32,
    /// имя файла строчными
    pub exe: String,
    pub path: Option<String>,
    /// байт/с чтения и записи, если считали
    pub io_rate: Option<f64>,
}

/// То, что видно не по списку процессов: служба, реестр, файлы.
#[derive(Default)]
pub struct Flags {
    pub wu_service: bool,
    pub steam_updating: bool,
    pub clips_module: bool,
    pub xbox_bg: bool,
}

/// Идущая игра: замеры копятся до её конца.
pub struct Live {
    pub title: String,
    pub started: Instant,
    pub inet: VecDeque<Option<u32>>,
    pub gw: VecDeque<Option<u32>>,
    pub seen: Vec<Kind>,
}

/// Сводка законченной игры.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub title: String,
    pub secs: u64,
    pub ended_at: String,
    pub inet: Option<Stats>,
    pub gw: Option<Stats>,
    /// None — успешных замеров нет
    pub level: Option<Level>,
    pub place: Place,
    pub spikes: usize,
    pub worst: Option<u32>,
    pub blame: Option<Kind>,
    /// 60 точек, максимум по корзине
    pub hist: Vec<Option<u32>>,
}

#[derive(Default)]
pub struct State {
    /// страница «Игра» на экране — выставляет интерфейс
    pub visible: bool,
    /// для снимков: образцы не перезаписываются потоками
    pub frozen: bool,
    /// шлюз (обычно роутер) и индекс адреса интернета в INET_TARGETS
    pub gw: Option<Ipv4Addr>,
    pub target: usize,
    pub gw_ring: VecDeque<Option<u32>>,
    pub inet_ring: VecDeque<Option<u32>>,
    /// Windows не дала открыть ICMP
    pub icmp_failed: bool,
    pub culprits: Vec<Culprit>,
    pub scanned: bool,
    /// Some — идёт игра, остров молчит
    pub session: Option<Live>,
    /// только сессии ≥ MIN_GAME_SECS
    pub last: Option<Summary>,
    /// только что закончились — плашка
    pub events: Vec<Summary>,
}

/// Машина состояний игровой сессии (живёт в потоке наблюдения, не в State).
#[derive(Default)]
pub struct Tracker {
    live: Option<Track>,
}

struct Track {
    pid: u32,
    /// строчными
    exe: String,
    started: Instant,
    away: Option<Instant>,
}

/// Что видно в эту секунду.
pub struct Seen<'a> {
    pub fullscreen: bool,
    /// 0 — нет
    pub fg_pid: u32,
    /// имя файла переднего окна (регистр любой), может быть "" — путь не прочитался
    pub fg_exe: &'a str,
    /// жив ли процесс отслеживаемой игры
    pub alive: bool,
}

#[derive(Debug, PartialEq)]
pub enum Step {
    Nothing,
    Started { pid: u32, exe: String },
    Ended { exe: String, secs: u64 },
}

/// Строка списка «Что может мешать игре».
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub glyph: &'static str,
    pub title: String,
    pub detail: String,
    pub hint: &'static str,
    pub open: Option<Open>,
    /// сетевая помеха — значок оранжевый
    pub network: bool,
}

// ───────────── чистая логика ─────────────

impl Stats {
    /// Доля потерь, %.
    pub fn loss_pct(&self) -> f32 {
        if self.sent == 0 {
            0.0
        } else {
            self.lost as f32 * 100.0 / self.sent as f32
        }
    }

    /// Сколько замеров получили ответ.
    pub fn ok(&self) -> usize {
        self.sent.saturating_sub(self.lost)
    }
}

/// Добавить замер в конец, держа длину не больше cap.
pub fn push_ring(r: &mut VecDeque<Option<u32>>, v: Option<u32>, cap: usize) {
    r.push_back(v);
    while r.len() > cap {
        r.pop_front();
    }
}

/// Медиана; при чётном числе — среднее двух средних. Пусто — 0.
fn median(ok: &[u32]) -> u32 {
    if ok.is_empty() {
        return 0;
    }
    let mut s = ok.to_vec();
    s.sort_unstable();
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        ((s[n / 2 - 1] as u64 + s[n / 2] as u64) / 2) as u32
    }
}

/// Сводка замеров: None, если замеров нет совсем. Всё, кроме потерь, считается по успешным.
pub fn stats(s: &[Option<u32>]) -> Option<Stats> {
    if s.is_empty() {
        return None;
    }
    let ok: Vec<u32> = s.iter().flatten().copied().collect();
    let (sent, lost) = (s.len(), s.len() - ok.len());
    if ok.is_empty() {
        return Some(Stats { sent, lost, ..Default::default() });
    }
    let avg = ok.iter().map(|&x| x as f64).sum::<f64>() / ok.len() as f64;
    // джиттер — средняя разница соседних успешных замеров, потери пропускаются
    let jitter = if ok.len() < 2 { 0.0 } else { ok.windows(2).map(|w| (w[1] as f64 - w[0] as f64).abs()).sum::<f64>() / (ok.len() - 1) as f64 };
    Some(Stats { sent, lost, avg: avg as f32, min: *ok.iter().min()?, max: *ok.iter().max()?, median: median(&ok), jitter: jitter as f32 })
}

/// «≤ a — зелёный, ≤ b — жёлтый, иначе красный».
fn grade<T: PartialOrd>(v: T, (a, b): (T, T)) -> Level {
    if v <= a {
        Level::Green
    } else if v <= b {
        Level::Yellow
    } else {
        Level::Red
    }
}

/// Пинг оценивается по медиане: один скачок не портит оценку.
pub fn ping_level(s: &Stats, lim: &Limits) -> Level {
    grade(s.median, lim.ping)
}

pub fn jitter_level(s: &Stats, lim: &Limits) -> Level {
    grade(s.jitter, lim.jitter)
}

pub fn loss_level(s: &Stats, lim: &Limits) -> Level {
    grade(s.loss_pct(), lim.loss)
}

/// Худший из трёх: пинг, джиттер, потери.
pub fn level_of(s: &Stats, lim: &Limits) -> Level {
    ping_level(s, lim).max(jitter_level(s, lim)).max(loss_level(s, lim))
}

/// Оценка на странице: пока замеров меньше трёх — «собираю данные».
pub fn page_level(inet: Option<&Stats>) -> Option<Level> {
    inet.filter(|s| s.sent >= 3).map(|s| level_of(s, &INET))
}

/// Роутер, ни разу не ответивший при живом интернете, просто не отвечает на пинг — по нему не судим.
pub fn usable_gw(gw: &Stats, inet: &Stats) -> bool {
    !(gw.ok() == 0 && inet.ok() > 0)
}

/// Где проблема. Смотрим только то, из-за чего плох интернет (худшие из пинга, джиттера, потерь),
/// и считаем «дома», если до роутера та же беда хотя бы наполовину: половина пинга, половина джиттера
/// (и сам джиттер до роутера не в норме) или половина потерь. Уровни роутера и интернета напрямую
/// не сравниваем: у них разные шкалы — 15 мс до роутера уже «жёлтые», но не объясняют 80 мс до интернета.
/// Потери до роутера сами по себе не в счёт: роутер отвечает на пинг к себе в последнюю очередь
/// и теряет его, даже когда чужие пакеты пропускает исправно.
pub fn place(gw: Option<&Stats>, inet: &Stats) -> Place {
    let li = level_of(inet, &INET);
    if li == Level::Green {
        return Place::Fine;
    }
    let Some(g) = gw.filter(|g| usable_gw(g, inet)) else { return Place::Unknown };
    let ping = ping_level(inet, &INET) == li && g.median as u64 * 2 >= inet.median as u64;
    let jitter = jitter_level(inet, &INET) == li && jitter_level(g, &GATEWAY) != Level::Green && g.jitter * 2.0 >= inet.jitter;
    let loss = loss_level(inet, &INET) == li && g.lost > 0 && g.loss_pct() * 2.0 >= inet.loss_pct();
    if ping || jitter || loss {
        Place::Home
    } else {
        Place::Beyond
    }
}

/// Скачки пинга: замер не ниже «обычный + max(50, обычный)»; идущие подряд — один скачок.
/// Второе значение — самый высокий пинг среди скачков.
pub fn spikes(s: &[Option<u32>]) -> (usize, Option<u32>) {
    let ok: Vec<u32> = s.iter().flatten().copied().collect();
    if ok.is_empty() {
        return (0, None);
    }
    let base = median(&ok);
    let line = base.saturating_add(SPIKE_MIN_MS.max(base));
    let (mut n, mut worst, mut inside) = (0usize, None::<u32>, false);
    // потери пропускаются: потеря посреди скачка его не разрывает
    for &rtt in s.iter().flatten() {
        if rtt >= line {
            if !inside {
                n += 1;
                inside = true;
            }
            worst = Some(worst.map_or(rtt, |w| w.max(rtt)));
        } else {
            inside = false;
        }
    }
    (n, worst)
}

/// Верх шкалы графика: первая ступень, в которую максимум влезает с запасом в четверть.
pub fn graph_scale(max_ms: u32) -> u32 {
    let need = max_ms as f32 * 1.25;
    [50, 100, 150, 200, 300, 500, 1000].into_iter().find(|&s| s as f32 >= need).unwrap_or(1000)
}

/// Сжать замеры до n точек: в каждой корзине — самый высокий пинг (скачок не теряется) или None.
pub fn downsample_max(s: &[Option<u32>], n: usize) -> Vec<Option<u32>> {
    if n == 0 {
        return Vec::new();
    }
    if s.len() <= n {
        return s.to_vec();
    }
    (0..n)
        .map(|i| {
            let (a, b) = (i * s.len() / n, (i + 1) * s.len() / n);
            s[a..b.max(a + 1)].iter().flatten().max().copied()
        })
        .collect()
}

/// «0%», «<0,1%», «1,7%», «4%», «12%».
pub fn fmt_pct(p: f32) -> String {
    if p.is_nan() || p <= 0.0 {
        return "0%".into();
    }
    if p < 0.05 {
        return "<0,1%".into();
    }
    if p < 10.0 {
        let r = (p * 10.0).round() / 10.0;
        if r.fract() == 0.0 {
            format!("{r:.0}%")
        } else {
            format!("{r:.1}%").replace('.', ",")
        }
    } else {
        format!("{p:.0}%")
    }
}

/// «640 КБ/с», «3,2 МБ/с».
pub fn fmt_rate(bps: f64) -> String {
    const MB: f64 = 1_048_576.0;
    if bps >= MB {
        format!("{:.1} МБ/с", bps / MB).replace('.', ",")
    } else {
        format!("{:.0} КБ/с", bps / 1024.0)
    }
}

/// «—», «<1 мс», «22 мс».
pub fn fmt_ms(v: Option<u32>) -> String {
    match v {
        None => "—".into(),
        Some(0) => "<1 мс".into(),
        Some(ms) => format!("{ms} мс"),
    }
}

pub fn level_rgb(l: Option<Level>) -> [u8; 3] {
    match l {
        Some(Level::Green) => [0x7B, 0xE0, 0x9A],
        Some(Level::Yellow) => [0xFF, 0xB3, 0x40],
        Some(Level::Red) => [0xFF, 0x6B, 0x6B],
        None => [0xC4, 0xC8, 0xD4],
    }
}

pub fn level_word(l: Option<Level>) -> &'static str {
    match l {
        Some(Level::Green) => "Отлично",
        Some(Level::Yellow) => "Терпимо",
        Some(Level::Red) => "Плохо",
        None => "Собираю данные…",
    }
}

/// Две строки вердикта простыми словами: где проблема и что это значит.
pub fn place_lines(p: Place) -> (&'static str, &'static str) {
    match p {
        Place::Fine => ("сеть в порядке", "можно играть"),
        Place::Home => ("проблема дома", "Wi‑Fi или кабель до роутера"),
        Place::Beyond => ("проблема в интернете", "у провайдера или канал забит"),
        Place::Unknown => ("причина не ясна", "роутер не отвечает на пинг"),
    }
}

/// Адрес в сетевом порядке байт — так его ждут IcmpSendEcho и GetBestRoute.
pub fn ip_to_raw(ip: Ipv4Addr) -> u32 {
    u32::from_ne_bytes(ip.octets())
}

pub fn raw_to_ip(raw: u32) -> Ipv4Addr {
    Ipv4Addr::from(raw.to_ne_bytes())
}

/// Адрес интернета молчит с самого начала (провайдер режет ICMP) — после 5 потерь подряд берём следующий.
pub fn next_target(cur: usize, ever_ok: bool, fails_in_row: u32) -> usize {
    if !ever_ok && fails_in_row >= 5 {
        (cur + 1) % INET_TARGETS.len()
    } else {
        cur % INET_TARGETS.len()
    }
}

/// Замер стоял дольше 5 с — старые точки не рисуем.
pub fn should_reset(idle_secs: Option<f64>) -> bool {
    idle_secs.is_some_and(|s| s > 5.0)
}

/// Пингуем только когда страница открыта или идёт игра.
pub fn ping_wanted(visible: bool, in_game: bool, frozen: bool) -> bool {
    !frozen && (visible || in_game)
}

/// Как часто осматривать процессы: страница открыта — часто, в игре — редко, иначе никогда.
pub fn scan_every(visible: bool, in_game: bool) -> Option<Duration> {
    if visible {
        Some(SCAN_VISIBLE)
    } else if in_game {
        Some(SCAN_GAME)
    } else {
        None
    }
}

/// Полноэкранные программы, которые точно не игры (имена без .exe, строчными).
const NOT_GAMES: &[&str] = &[
    // браузеры
    "chrome", "msedge", "firefox", "opera", "brave", "browser", "vivaldi", "arc", "iexplore", "librewolf", "waterfox", "thorium", "chromium", "floorp", "msedgewebview2",
    // плееры
    "vlc", "mpc-hc", "mpc-hc64", "mpc-be", "mpc-be64", "potplayer", "potplayer64", "potplayermini", "potplayermini64", "kmplayer", "kmplayer64x", "mpv", "smplayer", "wmplayer", "video.ui", "microsoft.media.player",
    // прочее
    "applicationframehost", "explorer", "powerpnt", "pptview", "winword", "excel", "acrord32", "acrobat", "sumatrapdf", "mstsc", "msrdc", "vmware-vmx", "vmware", "virtualboxvm", "vmconnect", "obs64", "obs32", "code", "windowsterminal", "devenv", "telegram", "discord", "zoom", "teams", "ms-teams", "steam", "steamwebhelper", "photos", "microsoft.photos", "lockapp", "logonui",
];

/// Имя без «.exe» (регистр расширения любой).
fn stem(exe: &str) -> &str {
    let e = exe.trim();
    let cut = e.len().saturating_sub(4);
    if e.len() >= 4 && e.is_char_boundary(cut) && e[cut..].eq_ignore_ascii_case(".exe") {
        &e[..cut]
    } else {
        e
    }
}

/// Точно не игра (браузер, плеер, офис…). Пустое имя — не «не игра»: путь не прочитался, это может быть игра под античитом.
pub fn not_a_game(exe: &str) -> bool {
    let s = stem(exe).to_lowercase();
    !s.is_empty() && NOT_GAMES.contains(&s.as_str())
}

/// Знакомая сетевая игра с поиском матча (её мигание на панели задач — «матч найден»).
/// Чужие мигания (мессенджеры, загрузки) сюда не попадают.
pub fn is_matchmaking_game(exe: &str) -> bool {
    matches!(
        stem(exe).to_lowercase().as_str(),
        "cs2" | "csgo" | "dota2" | "valorant-win64-shipping" | "r5apex" | "r5apex_dx12" | "fortniteclient-win64-shipping" | "overwatch" | "tslgame" | "deadlock" | "leagueclientux"
    )
}

#[cfg(test)]
mod matchmaking_tests {
    #[test]
    fn only_known_games_flash_as_match() {
        use super::is_matchmaking_game as g;
        assert!(g("cs2.exe"));
        assert!(g("CS2.EXE"));
        assert!(g("dota2.exe"));
        assert!(!g("Discord.exe"));
        assert!(!g("Telegram.exe"));
        assert!(!g(""));
    }
}

/// Название игры для сводки: знакомые — красиво, остальные — по имени файла.
pub fn game_title(exe: &str) -> String {
    let s = stem(exe);
    let known = match s.to_lowercase().as_str() {
        "cs2" => "CS2",
        "csgo" => "CS:GO",
        "dota2" => "Dota 2",
        "valorant-win64-shipping" => "Valorant",
        "r5apex" => "Apex Legends",
        "fortniteclient-win64-shipping" => "Fortnite",
        "overwatch" => "Overwatch 2",
        "rustclient" => "Rust",
        "tslgame" => "PUBG",
        "deadlock" => "Deadlock",
        _ => "",
    };
    if !known.is_empty() {
        known.into()
    } else if s.is_empty() {
        "Игра".into()
    } else {
        crate::sys::pretty_name(s)
    }
}

impl Tracker {
    /// Одна секунда наблюдения: началась ли игра, вернулись ли в неё, закончилась ли.
    pub fn tick(&mut self, now: Instant, seen: &Seen) -> Step {
        let Some(t) = self.live.as_mut() else {
            if seen.fullscreen && seen.fg_pid != 0 && !not_a_game(seen.fg_exe) {
                self.live = Some(Track { pid: seen.fg_pid, exe: seen.fg_exe.to_lowercase(), started: now, away: None });
                return Step::Started { pid: seen.fg_pid, exe: seen.fg_exe.to_string() };
            }
            return Step::Nothing;
        };
        let end = if !seen.alive {
            // закрыли, пока были вне игры, — время вне игры не считаем
            Some(t.away.unwrap_or(now))
        } else if seen.fullscreen && seen.fg_pid == t.pid {
            t.away = None;
            None
        } else {
            let since = *t.away.get_or_insert(now);
            (now.saturating_duration_since(since).as_secs() >= AWAY_SECS).then_some(since)
        };
        match end {
            Some(end) => {
                let secs = end.saturating_duration_since(t.started).as_secs();
                let exe = std::mem::take(&mut t.exe);
                self.live = None;
                Step::Ended { exe, secs }
            }
            None => Step::Nothing,
        }
    }

    /// Наблюдение стояло с `since` дольше, чем можно быть вне игры (компьютер спал): игра кончилась тогда,
    /// время сна в неё не идёт. Если игра идёт и после сна, следующий тик начнёт новую.
    pub fn interrupted(&mut self, since: Instant) -> Step {
        match self.live.take() {
            Some(t) => Step::Ended { secs: t.away.unwrap_or(since).saturating_duration_since(t.started).as_secs(), exe: t.exe },
            None => Step::Nothing,
        }
    }

    /// pid и имя (строчными) отслеживаемой игры.
    pub fn pid(&self) -> Option<(u32, &str)> {
        self.live.as_ref().map(|t| (t.pid, t.exe.as_str()))
    }

    pub fn active(&self) -> bool {
        self.live.is_some()
    }
}

/// Между тиками наблюдения прошло не меньше AWAY_SECS — компьютер спал (тики идут раз в секунду).
pub fn stalled(prev: Instant, now: Instant) -> bool {
    now.saturating_duration_since(prev).as_secs() >= AWAY_SECS
}

/// Короче трёх минут — не игра (видео, случайное окно на весь экран): без сводки.
pub fn worth_summary(secs: u64) -> bool {
    secs >= MIN_GAME_SECS
}

impl Live {
    pub fn new(exe: &str, now: Instant) -> Live {
        Live { title: game_title(exe), started: now, inet: VecDeque::new(), gw: VecDeque::new(), seen: Vec::new() }
    }

    /// gw: None — шлюз неизвестен, замер роутера не пишем.
    pub fn push(&mut self, inet: Option<u32>, gw: Option<Option<u32>>) {
        push_ring(&mut self.inet, inet, MAX_SESSION_SAMPLES);
        if let Some(g) = gw {
            push_ring(&mut self.gw, g, MAX_SESSION_SAMPLES);
        }
    }

    /// Запомнить, какие помехи были за игру (только явные: косвенные признаки винить не будем).
    pub fn note(&mut self, list: &[Culprit]) {
        for c in list.iter().filter(|c| c.sure) {
            if !self.seen.contains(&c.kind) {
                self.seen.push(c.kind);
            }
        }
    }
}

/// Помеха, которая забирает канал (её можно винить в плохом пинге).
pub fn hits_network(k: Kind) -> bool {
    matches!(k, Kind::Updates | Kind::Torrent | Kind::Steam | Kind::Launcher | Kind::OneDrive)
}

/// Кого винить в плохой сети: самую приоритетную из сетевых помех за игру.
pub fn blame(seen: &[Kind]) -> Option<Kind> {
    seen.iter().copied().filter(|k| hits_network(*k)).min()
}

/// Кто, похоже, мешает сейчас: самая приоритетная из явных сетевых помех в списке.
/// Запущенная служба обновлений или флаг Steam в реестре — не улика: служба запускается сама и работает часто.
pub fn suspect(list: &[Culprit]) -> Option<Kind> {
    list.iter().filter(|c| c.sure && hits_network(c.kind)).map(|c| c.kind).min()
}

pub fn kind_glyph(k: Kind) -> &'static str {
    match k {
        Kind::Updates => "\u{E895}",
        Kind::Torrent => "\u{E896}",
        Kind::Steam | Kind::Launcher => "\u{E7FC}",
        Kind::OneDrive => "\u{E753}",
        Kind::DiscordClips => "\u{E8AA}",
        Kind::NvidiaReplay => "\u{E714}",
        Kind::XboxDvr => "\u{E722}",
        Kind::Build => "\u{E943}",
    }
}

/// Короткое имя для «мешал …» (всё мужского рода).
pub fn kind_short(k: Kind) -> &'static str {
    match k {
        Kind::Updates => "Windows Update",
        Kind::Torrent => "торрент-клиент",
        Kind::Steam => "Steam",
        Kind::Launcher => "лаунчер",
        Kind::OneDrive => "OneDrive",
        Kind::DiscordClips => "Discord Clips",
        Kind::NvidiaReplay => "NVIDIA",
        Kind::XboxDvr => "Xbox Game Bar",
        Kind::Build => "сборка",
    }
}

/// Как выключить вручную: остров сам ничего не выключает.
pub fn kind_hint(k: Kind) -> &'static str {
    match k {
        Kind::Updates => "Параметры → Центр обновления → Приостановить; в «Оптимизации доставки» запретите отдачу другим ПК",
        Kind::Torrent => "Раздачи забивают отдачу — пауза или лимит скорости в клиенте",
        Kind::Steam => "Загрузки → пауза; в настройках снимите «Разрешать загрузки во время игры»",
        Kind::Launcher => "Поставьте загрузку на паузу в лаунчере",
        Kind::OneDrive => "Значок OneDrive в трее → шестерёнка → Приостановить синхронизацию",
        Kind::DiscordClips => "Discord → Настройки → Клипы → выключить запись",
        Kind::NvidiaReplay => "Alt+Z → Запись → Мгновенный повтор → выключить",
        Kind::XboxDvr => "Параметры → Игры → Клипы → выключить запись в фоновом режиме",
        Kind::Build => "Сборка занимает все ядра — FPS проседает; дождитесь конца",
    }
}

/// Процессы установки обновлений Windows и как их показать.
const WU_PROCS: &[(&str, &str)] = &[("windowsupdatebox.exe", "WindowsUpdateBox"), ("tiworker.exe", "TiWorker"), ("mousocoreworker.exe", "MoUsoCoreWorker"), ("wuauclt.exe", "wuauclt")];
/// Торрент-клиенты и менеджеры загрузок: имя файла, название, есть ли окно.
const TORRENTS: &[(&str, &str, bool)] = &[
    ("qbittorrent.exe", "qBittorrent", true),
    ("utorrent.exe", "µTorrent", true),
    ("bittorrent.exe", "BitTorrent", true),
    ("transmission-qt.exe", "Transmission", true),
    ("transmission-daemon.exe", "Transmission", false),
    ("tixati.exe", "Tixati", true),
    ("deluge.exe", "Deluge", true),
    ("deluged.exe", "Deluge", false),
    ("biglybt.exe", "BiglyBT", true),
    ("mediaget.exe", "MediaGet", true),
    ("azureus.exe", "Vuze", true),
    ("frostwire.exe", "FrostWire", true),
    ("picotorrent.exe", "PicoTorrent", true),
    ("tribler.exe", "Tribler", true),
    ("motrix.exe", "Motrix", true),
    ("aria2c.exe", "aria2", false),
    ("idman.exe", "IDM", true),
    ("fdm.exe", "Free Download Manager", true),
];
const LAUNCHERS: &[(&str, &str)] = &[("battle.net.exe", "Battle.net"), ("epicgameslauncher.exe", "Epic Games"), ("eadesktop.exe", "EA app"), ("upc.exe", "Ubisoft Connect"), ("galaxyclient.exe", "GOG Galaxy")];
const DISCORD: &[&str] = &["discord.exe", "discordptb.exe", "discordcanary.exe"];
const NVIDIA: &[&str] = &["nvidia overlay.exe", "nvidia share.exe", "nvsphelper64.exe", "nvsphelper.exe"];
const NVIDIA_APPS: &[&str] = &["nvidia app.exe", "nvidia geforce experience.exe"];
const BUILD: &[&str] = &["cargo.exe", "rustc.exe", "link.exe", "cl.exe", "msbuild.exe", "ninja.exe", "clang.exe", "clang++.exe", "gcc.exe", "g++.exe", "cc1plus.exe"];

/// Следим ли за этим процессом (имя строчными, с .exe).
pub fn is_watched(exe: &str) -> bool {
    WU_PROCS.iter().any(|w| w.0 == exe)
        || TORRENTS.iter().any(|t| t.0 == exe)
        || LAUNCHERS.iter().any(|l| l.0 == exe)
        || DISCORD.contains(&exe)
        || NVIDIA.contains(&exe)
        || NVIDIA_APPS.contains(&exe)
        || BUILD.contains(&exe)
        || exe == "steam.exe"
        || exe == "onedrive.exe"
}

/// Нужен ли счётчик ввода-вывода (качает ли программа сейчас).
pub fn io_watched(exe: &str) -> bool {
    exe == "onedrive.exe" || exe == "steam.exe" || LAUNCHERS.iter().any(|l| l.0 == exe)
}

/// Прошлый замер ввода-вывода старше этого (страницу закрывали, игры не было) — скорость по нему не считаем:
/// среднее за десять минут покажет «качает» про синхронизацию, давно закончившуюся.
pub const IO_MAX_GAP: f64 = 45.0;

/// Скорость ввода-вывода по двум замерам; None — прошлого нет, промежуток короткий или давний, счётчик сбросился.
pub fn io_rate(prev: Option<u64>, cur: u64, dt: f64) -> Option<f64> {
    let prev = prev?;
    ((0.5..=IO_MAX_GAP).contains(&dt) && cur >= prev).then(|| (cur - prev) as f64 / dt)
}

/// Что может мешать игре: не больше одного пункта на вид, по приоритету.
pub fn detect(procs: &[Proc], f: &Flags) -> Vec<Culprit> {
    let find = |exe: &str| procs.iter().find(|p| p.exe == exe);
    let any_of = |list: &'static [&'static str]| procs.iter().filter(move |p| list.contains(&p.exe.as_str()));
    let rate_of = |exe: &str| procs.iter().filter(|p| p.exe == exe).filter_map(|p| p.io_rate).fold(None, |a: Option<f64>, r| Some(a.map_or(r, |a| a.max(r))));
    let mut out = Vec::new();

    // обновления Windows: установку видно по процессам, загрузку — только по службе
    let wu: Vec<&str> = WU_PROCS.iter().filter(|w| find(w.0).is_some()).map(|w| w.1).collect();
    if !wu.is_empty() || f.wu_service {
        let mut parts: Vec<String> = wu.iter().map(|s| s.to_string()).collect();
        if f.wu_service {
            parts.push("служба wuauserv".into());
        }
        let title = if wu.is_empty() { "Обновления Windows могут качаться" } else { "Устанавливаются обновления Windows" };
        // одна служба — не улика: она запускается по расписанию и работает часто
        out.push(Culprit { kind: Kind::Updates, title: title.into(), detail: parts.join(", "), open: Some(Open::Uri("ms-settings:windowsupdate")), sure: !wu.is_empty() });
    }

    if let Some((exe, name, window, p)) = TORRENTS.iter().find_map(|&(exe, name, window)| find(exe).map(|p| (exe, name, window, p))) {
        let manager = matches!(exe, "idman.exe" | "fdm.exe");
        out.push(Culprit {
            kind: Kind::Torrent,
            title: format!("{} {name}", if manager { "Менеджер загрузок" } else { "Торрент-клиент" }),
            detail: if manager { "загрузки забивают канал" } else { "раздачи забивают канал" }.into(),
            // у службы без окна открывать нечего
            open: if window { p.path.clone().map(Open::Exe) } else { None },
            sure: true,
        });
    }

    if find("steam.exe").is_some() {
        let rate = rate_of("steam.exe").filter(|r| *r >= STEAM_IO);
        // флаг «обновляется» в реестре Steam залипает: без скорости это только очередь
        let (title, detail) = match rate {
            Some(r) => ("Steam качает обновление", fmt_rate(r)),
            None => ("Steam: обновление в очереди", "может начать качать прямо в игре".into()),
        };
        if f.steam_updating || rate.is_some() {
            out.push(Culprit { kind: Kind::Steam, title: title.into(), detail, open: Some(Open::Uri("steam://open/downloads")), sure: rate.is_some() });
        }
    }

    if let Some((name, rate, p)) = LAUNCHERS.iter().find_map(|&(exe, name)| Some((name, rate_of(exe).filter(|r| *r >= LAUNCHER_IO)?, find(exe)?))) {
        out.push(Culprit { kind: Kind::Launcher, title: format!("{name} качает"), detail: fmt_rate(rate), open: p.path.clone().map(Open::Exe), sure: true });
    }

    if let Some(rate) = rate_of("onedrive.exe").filter(|r| *r >= ONEDRIVE_IO) {
        out.push(Culprit { kind: Kind::OneDrive, title: "OneDrive синхронизирует".into(), detail: fmt_rate(rate), open: find("onedrive.exe").and_then(|p| p.path.clone()).map(Open::Exe), sure: true });
    }

    if f.clips_module && any_of(DISCORD).next().is_some() {
        let path = any_of(DISCORD).find_map(|p| p.path.clone());
        out.push(Culprit { kind: Kind::DiscordClips, title: "Discord: запись клипов".into(), detail: "модуль клипов установлен — проверьте, выключена ли запись".into(), open: path.map(Open::Exe), sure: false });
    }

    if any_of(NVIDIA).next().is_some() {
        let app = any_of(NVIDIA_APPS).find_map(|p| p.path.clone());
        out.push(Culprit { kind: Kind::NvidiaReplay, title: "NVIDIA: оверлей и мгновенный повтор".into(), detail: "оверлей запущен — проверьте, выключен ли повтор".into(), open: app.map(Open::Exe), sure: false });
    }

    if f.xbox_bg {
        out.push(Culprit { kind: Kind::XboxDvr, title: "Xbox Game Bar пишет игру в фоне".into(), detail: "запись последних минут включена".into(), open: Some(Open::Uri("ms-settings:gaming-gamedvr")), sure: true });
    }

    let mut tools: Vec<String> = Vec::new();
    for p in any_of(BUILD) {
        let n = crate::sys::pretty_name(&p.exe);
        if !tools.contains(&n) {
            tools.push(n);
        }
    }
    if !tools.is_empty() {
        tools.truncate(3);
        out.push(Culprit { kind: Kind::Build, title: "Идёт сборка в фоне".into(), detail: tools.join(", "), open: Some(Open::Uri("taskmgr")), sure: true });
    }

    out.sort_by_key(|c| c.kind);
    out
}

/// Xbox Game Bar пишет последние минуты игры: запись не выключена, захват приложений и фоновая запись включены.
pub fn xbox_bg(dvr_enabled: Option<u32>, app_capture: Option<u32>, historical: Option<u32>) -> bool {
    dvr_enabled != Some(0) && app_capture == Some(1) && historical == Some(1)
}

/// Среди модулей Discord есть модуль клипов.
pub fn has_clips_module(names: &[String]) -> bool {
    names.iter().any(|n| n.to_lowercase().starts_with("discord_clips"))
}

/// Путь программы, который можно открыть: локальный диск, .exe, без кавычек, сетевых путей и управляющих символов.
/// Двоеточие только после буквы диска: «C:\a\b.txt:x.exe» — поток NTFS внутри другого файла, а не программа.
/// Без «%»: путь проверяется как есть, а оболочка могла бы подставить в него переменные окружения.
pub fn safe_exe(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() > 7
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && b[2] == b'\\'
        && !path.starts_with(r"\\")
        && !path[2..].contains(':')
        && !path.contains('"')
        && !path.contains('%')
        && !path.chars().any(char::is_control)
        && path.to_ascii_lowercase().ends_with(".exe")
}

/// Что передать оболочке для адреса со страницы: только адреса из ALLOWED_URIS, иначе None.
/// Диспетчер задач — полным путём из системной папки: по одному имени ShellExecute сначала ищет в текущей
/// папке острова (скажем, в «Загрузках», если остров запустили оттуда), и подложенный там taskmgr.exe/.com/.bat
/// запустился бы вместо настоящего. Системную папку не узнали — не открываем ничего.
pub fn launch_target(u: &str) -> Option<String> {
    if !ALLOWED_URIS.contains(&u) {
        return None;
    }
    if u == "taskmgr" {
        return system_dir().map(|d| taskmgr_path(&d)).filter(|p| safe_exe(p));
    }
    Some(u.to_string())
}

/// Путь диспетчера задач в системной папке.
fn taskmgr_path(system_dir: &str) -> String {
    format!(r"{}\Taskmgr.exe", system_dir.trim_end_matches('\\'))
}

/// Системная папка Windows (обычно C:\Windows\System32) — у ядра, а не из переменной окружения.
fn system_dir() -> Option<String> {
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    let mut buf = [0u16; 260];
    // SAFETY: буфер жив на время вызова, длину функция берёт из среза
    let n = unsafe { GetSystemDirectoryW(Some(&mut buf)) } as usize;
    // 0 — ошибка; не меньше длины буфера — не влезло (возвращён нужный размер)
    (n > 0 && n < buf.len()).then(|| String::from_utf16_lossy(&buf[..n]))
}

/// Подпись кнопки.
pub fn open_label(o: &Open) -> &'static str {
    match o {
        Open::Uri(u) if u.starts_with("ms-settings:") => "Параметры",
        Open::Uri(u) if u.starts_with("steam://") => "Загрузки",
        Open::Uri("taskmgr") => "Диспетчер",
        _ => "Открыть",
    }
}

/// Подсказка к кнопке: куда она ведёт.
pub fn open_tip(o: &Open) -> String {
    match o {
        Open::Uri("ms-settings:windowsupdate") => "Открыть «Центр обновления Windows»".into(),
        Open::Uri("steam://open/downloads") => "Открыть загрузки Steam".into(),
        Open::Uri("ms-settings:gaming-gamedvr") => "Открыть «Игры → Клипы» в параметрах".into(),
        Open::Uri("taskmgr") => "Открыть диспетчер задач".into(),
        Open::Uri("ms-settings:storagesense") => "Открыть «Память устройства» — там видно, чем занят диск".into(),
        Open::Uri(u) => format!("Открыть {u}"),
        Open::Exe(p) => format!("Открыть {}", p.rsplit('\\').next().unwrap_or(p)),
    }
}

/// Мало места на диске C. До первого замера (нули) тревоги нет.
pub fn low_disk(free_gb: f64, total_gb: f64) -> bool {
    total_gb > 0.0 && free_gb >= 0.0 && free_gb < LOW_DISK_GB
}

/// Строки списка: помехи по приоритету, затем диск, если места мало.
pub fn rows(culprits: &[Culprit], free_gb: f64, total_gb: f64) -> Vec<Row> {
    let mut out: Vec<Row> = culprits.iter().map(|c| Row { glyph: kind_glyph(c.kind), title: c.title.clone(), detail: c.detail.clone(), hint: kind_hint(c.kind), open: c.open.clone(), network: hits_network(c.kind) }).collect();
    if low_disk(free_gb, total_gb) {
        out.push(Row {
            glyph: "\u{EDA2}",
            title: "Мало места на диске C".into(),
            detail: format!("свободно {free_gb:.0} ГБ — игре и обновлениям нужно место"),
            hint: "Параметры → Система → Память: удалите лишнее или включите контроль памяти",
            open: Some(Open::Uri("ms-settings:storagesense")),
            network: false,
        });
    }
    out
}

/// Сводка законченной игры по собранным замерам.
pub fn summarize(live: &Live, secs: u64, ended_at: String) -> Summary {
    let inet_v: Vec<Option<u32>> = live.inet.iter().copied().collect();
    let gw_v: Vec<Option<u32>> = live.gw.iter().copied().collect();
    let (inet, gw) = (stats(&inet_v), stats(&gw_v));
    let level = inet.filter(|s| s.ok() > 0).map(|s| level_of(&s, &INET));
    let pl = match (inet, level) {
        (Some(i), Some(_)) => place(gw.as_ref(), &i),
        _ => Place::Unknown,
    };
    let (n, worst) = spikes(&inet_v);
    // винить кого-то стоит, только если сеть была не в порядке
    let who = if level.is_some_and(|l| l != Level::Green) { blame(&live.seen) } else { None };
    Summary { title: live.title.clone(), secs, ended_at, inet, gw, level, place: pl, spikes: n, worst, blame: who, hist: downsample_max(&inet_v, 60) }
}

/// Главное, что было не так.
fn headline(s: &Summary, i: &Stats) -> String {
    let (lp, lj, ll) = (ping_level(i, &INET), jitter_level(i, &INET), loss_level(i, &INET));
    if ll == Level::Red || (i.lost > 0 && ll >= lp && ll >= lj) {
        format!("Игра: терялись пакеты — {}", fmt_pct(i.loss_pct()))
    } else if s.spikes > 0 {
        format!("Игра: были скачки пинга до {} мс", s.worst.unwrap_or(i.max))
    } else if lp >= lj {
        format!("Игра: высокий пинг — {} мс", i.median)
    } else {
        format!("Игра: пинг скакал — джиттер {:.0} мс", i.jitter)
    }
}

/// Почему так: кто мешал или где проблема (None — не знаем).
fn reason(s: &Summary) -> Option<String> {
    match (s.blame, s.place) {
        (Some(k), _) => Some(format!("мешал {}", kind_short(k))),
        (None, Place::Home) => Some("дома: Wi‑Fi или кабель до роутера".into()),
        (None, Place::Beyond) => Some("в интернете: провайдер или канал забит".into()),
        _ => None,
    }
}

/// Плашка после игры: заголовок до 44 символов, текст до 56 (столько показывает остров).
pub fn summary_toast(s: &Summary) -> (String, String) {
    let dur = crate::pult::fmt_duration(s.secs.min(u32::MAX as u64) as u32);
    let (Some(level), Some(i)) = (s.level, s.inet) else {
        return (format!("Игра окончена · {dur}"), "пинг не измерен — интернет не отвечал".into());
    };
    if level == Level::Green {
        return (format!("Игра: пинг {} мс, джиттер {:.0}, потери {}", i.median, i.jitter, fmt_pct(i.loss_pct())), format!("{dur} · сеть в порядке"));
    }
    let text = match (s.blame, reason(s)) {
        (Some(_), Some(r)) => format!("{r} · щёлкните — подробности"),
        (None, Some(r)) => r,
        _ => format!("{dur} · щёлкните — подробности"),
    };
    (headline(s, &i), text)
}

/// Строка итога для страницы.
pub fn summary_line(s: &Summary) -> String {
    let (Some(level), Some(i)) = (s.level, s.inet) else { return "пинг не измерен".into() };
    if level == Level::Green {
        return "сеть в порядке".into();
    }
    let head = headline(s, &i);
    let head = head.strip_prefix("Игра: ").unwrap_or(&head);
    match reason(s) {
        Some(r) => format!("{head} · {r}"),
        None => head.to_string(),
    }
}

/// Высота страницы «Игра» (в единицах таблицы panel_height): та же раскладка, что в ui_game.
/// 104 — навигация и поля панели; после каждого allocate идёт item_spacing 8; два add_space(2).
pub fn page_height(rows: usize, has_game: bool) -> f32 {
    let n = rows.min(MAX_ROWS);
    let list = if n == 0 { EMPTY_ROW_H } else { n as f32 * ROW_H + (n - 1) as f32 * GAP };
    104.0 + HERO_H + GAP + GRAPH_H + GAP + 2.0 + LIST_HEAD_H + GAP + list + GAP + 2.0 + if has_game { LAST_H } else { LAST_EMPTY_H }
}

/// Для снимков: образцы замеров, помех и последней игры (без случайностей).
pub fn fill_demo(s: &mut State) {
    s.gw = Some(Ipv4Addr::new(192, 168, 0, 1));
    s.target = 0;
    s.icmp_failed = false;
    s.inet_ring = (0..RING)
        .map(|i| match i {
            41 => Some(140),
            50 => None,
            _ => Some(18 + ((i * 7 + 3) % 9) as u32),
        })
        .collect();
    s.gw_ring = (0..RING).map(|i| Some(1 + ((i * 5) % 3) as u32)).collect();
    let procs = [
        Proc { pid: 1, exe: "tiworker.exe".into(), path: None, io_rate: None },
        Proc { pid: 2, exe: "steam.exe".into(), path: None, io_rate: None },
        Proc { pid: 3, exe: "discord.exe".into(), path: Some(r"C:\Users\user\AppData\Local\Discord\app-1.0.9175\Discord.exe".into()), io_rate: None },
    ];
    // Steam в образце — с флагом в реестре и без скорости: строка «обновление в очереди»
    s.culprits = detect(&procs, &Flags { wu_service: true, steam_updating: true, clips_module: true, xbox_bg: false });
    s.scanned = true;
    s.last = Some(demo_summary());
}

/// Образец сводки: CS2, час с небольшим, три скачка, мешал Windows Update.
pub fn demo_summary() -> Summary {
    let hist = (0..60u32)
        .map(|i| {
            Some(match i {
                14 => 310,
                33 => 180,
                47 => 150,
                _ => 20 + (i * 5 + 1) % 9,
            })
        })
        .collect();
    let inet = Stats { sent: 4320, lost: 0, avg: 23.4, min: 17, max: 310, median: 22, jitter: 6.4 };
    let gw = Stats { sent: 4320, lost: 0, avg: 2.1, min: 1, max: 9, median: 2, jitter: 0.8 };
    Summary {
        title: "CS2".into(),
        secs: 4320,
        ended_at: "21:40".into(),
        inet: Some(inet),
        gw: Some(gw),
        level: Some(level_of(&inet, &INET)),
        place: place(Some(&gw), &inet),
        spikes: 3,
        worst: Some(310),
        blame: Some(Kind::Updates),
        hist,
    }
}

// ───────────── Windows: ICMP, маршрут, процессы, служба, реестр ─────────────

/// ICMP-хэндл, закрывается в Drop.
struct Icmp(HANDLE);

impl Icmp {
    fn open() -> Option<Icmp> {
        unsafe { windows::Win32::NetworkManagement::IpHelper::IcmpCreateFile() }.ok().map(Icmp)
    }

    /// Один эхо-запрос: время ответа в мс или None (потеря, ошибка, ответ не от того адреса).
    fn echo(&self, to: Ipv4Addr, timeout_ms: u32) -> Option<u32> {
        use windows::Win32::NetworkManagement::IpHelper::{IcmpSendEcho, ICMP_ECHO_REPLY, IP_SUCCESS};
        // те же 32 байта, что шлёт ping.exe из Windows: по пакету не видно ни программы, ни того, что идёт игра
        const PAYLOAD: [u8; 32] = *b"abcdefghijklmnopqrstuvwabcdefghi";
        // ответ + копия данных + 8 байт на ICMP-ошибку; u64 — ради выравнивания структуры
        let words = (std::mem::size_of::<ICMP_ECHO_REPLY>() + PAYLOAD.len() + 8 + 7) / 8;
        let mut buf = vec![0u64; words];
        let data = PAYLOAD;
        // SAFETY: данные и буфер живы на время вызова, размеры переданы точно
        let n = unsafe { IcmpSendEcho(self.0, ip_to_raw(to), data.as_ptr().cast(), data.len() as u16, None, buf.as_mut_ptr().cast(), (words * 8) as u32, timeout_ms) };
        if n == 0 {
            return None;
        }
        // SAFETY: буфер не меньше ICMP_ECHO_REPLY; читаем только Address, Status, RoundTripTime
        let r: ICMP_ECHO_REPLY = unsafe { std::ptr::read_unaligned(buf.as_ptr().cast()) };
        (r.Status == IP_SUCCESS && r.Address == ip_to_raw(to)).then_some(r.RoundTripTime)
    }
}

impl Drop for Icmp {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::NetworkManagement::IpHelper::IcmpCloseHandle(self.0);
        }
    }
}

/// Шлюз по маршруту к 1.1.1.1 — обычно роутер (с VPN — его сервер). 0.0.0.0 — прямое подключение.
fn gateway() -> Option<Ipv4Addr> {
    use windows::Win32::NetworkManagement::IpHelper::{GetBestRoute, MIB_IPFORWARDROW};
    let mut row = MIB_IPFORWARDROW::default();
    let rc = unsafe { GetBestRoute(ip_to_raw(INET_TARGETS[0]), 0, &mut row) };
    let ip = raw_to_ip(row.dwForwardNextHop);
    (rc == 0 && !ip.is_unspecified()).then_some(ip)
}

/// Хэндл процесса или снимка процессов, закрывается в Drop.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Открыть процесс только на чтение сведений.
fn open_proc(pid: u32) -> Option<Owned> {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok().map(Owned)
}

fn path_of(h: &Owned) -> Option<String> {
    use windows::Win32::System::Threading::{QueryFullProcessImageNameW, PROCESS_NAME_WIN32};
    let mut buf = [0u16; 512];
    let mut len = buf.len() as u32;
    unsafe { QueryFullProcessImageNameW(h.0, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len) }.ok()?;
    Some(String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]))
}

/// Сколько байт процесс прочитал и записал (диск и сеть вместе).
fn io_of(h: &Owned) -> Option<u64> {
    use windows::Win32::System::Threading::{GetProcessIoCounters, IO_COUNTERS};
    let mut c = IO_COUNTERS::default();
    unsafe { GetProcessIoCounters(h.0, &mut c) }.ok()?;
    Some(c.ReadTransferCount.saturating_add(c.WriteTransferCount).saturating_add(c.OtherTransferCount))
}

fn exited(h: &Owned) -> bool {
    use windows::Win32::System::Threading::GetExitCodeProcess;
    let mut code = 0u32;
    unsafe { GetExitCodeProcess(h.0, &mut code) }.is_ok() && code != windows::Win32::Foundation::STILL_ACTIVE.0 as u32
}

/// Все процессы: pid и имя файла (как есть).
pub fn snapshot() -> Vec<(u32, String)> {
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    let mut out = Vec::new();
    let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else { return out };
    let snap = Owned(snap);
    let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
    unsafe {
        if Process32FirstW(snap.0, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
                out.push((e.th32ProcessID, String::from_utf16_lossy(&e.szExeFile[..len])));
                if out.len() >= 8192 || Process32NextW(snap.0, &mut e).is_err() {
                    break;
                }
            }
        }
    }
    out
}

/// Хэндл диспетчера служб или службы, закрывается в Drop.
struct Sc(windows::Win32::System::Services::SC_HANDLE);

impl Drop for Sc {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::System::Services::CloseServiceHandle(self.0);
        }
    }
}

/// Запущена ли служба Windows Update (только чтение состояния).
fn service_running() -> bool {
    use windows::core::{w, PCWSTR};
    use windows::Win32::System::Services::{OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS};
    unsafe {
        let Ok(scm) = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) else { return false };
        let scm = Sc(scm);
        let Ok(svc) = OpenServiceW(scm.0, w!("wuauserv"), SERVICE_QUERY_STATUS) else { return false };
        let svc = Sc(svc);
        let mut st = SERVICE_STATUS::default();
        QueryServiceStatus(svc.0, &mut st).is_ok() && st.dwCurrentState == SERVICE_RUNNING
    }
}

/// Steam отметил в реестре, что какая-то игра обновляется (реестр только читаем).
fn steam_updating() -> bool {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let Ok(apps) = RegKey::predef(HKEY_CURRENT_USER).open_subkey(r"Software\Valve\Steam\Apps") else { return false };
    apps.enum_keys().take(2000).flatten().any(|k| apps.open_subkey(&k).ok().and_then(|s| s.get_value::<u32, _>("Updating").ok()) == Some(1))
}

/// Установлен ли модуль клипов Discord (смотрим только имена папок).
fn clips_module_installed() -> bool {
    let Some(base) = std::env::var_os("LOCALAPPDATA") else { return false };
    let base = std::path::PathBuf::from(base);
    for app in ["Discord", "DiscordPTB", "DiscordCanary"] {
        let Ok(dirs) = std::fs::read_dir(base.join(app)) else { continue };
        for d in dirs.flatten().take(64) {
            if !d.file_name().to_string_lossy().starts_with("app-") {
                continue;
            }
            let Ok(mods) = std::fs::read_dir(d.path().join("modules")) else { continue };
            let names: Vec<String> = mods.flatten().take(256).map(|m| m.file_name().to_string_lossy().into_owned()).collect();
            if has_clips_module(&names) {
                return true;
            }
        }
    }
    false
}

/// Включена ли фоновая запись Xbox Game Bar (реестр только читаем).
fn xbox_bg_record() -> bool {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let hk = RegKey::predef(HKEY_CURRENT_USER);
    let get = |path: &str, name: &str| hk.open_subkey(path).ok().and_then(|k| k.get_value::<u32, _>(name).ok());
    const DVR: &str = r"Software\Microsoft\Windows\CurrentVersion\GameDVR";
    xbox_bg(get(r"System\GameConfigStore", "GameDVR_Enabled"), get(DVR, "AppCaptureEnabled"), get(DVR, "HistoricalCaptureEnabled"))
}

/// pid процесса переднего окна. Своё окно или окна нет — 0.
fn foreground_pid() -> u32 {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    let mut pid = 0u32;
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0.is_null() {
            return 0;
        }
        GetWindowThreadProcessId(fg, Some(&mut pid));
    }
    if pid == std::process::id() {
        0
    } else {
        pid
    }
}

/// Имя файла процесса (регистр как есть): из полного пути, а если процесс закрыт от чтения (античит) —
/// из списка процессов, где имя видно всегда. Не нашлось — "".
fn exe_name(pid: u32) -> String {
    open_proc(pid)
        .and_then(|h| path_of(&h))
        .and_then(|p| p.rsplit('\\').next().map(str::to_string))
        .or_else(|| snapshot().into_iter().find(|(p, _)| *p == pid).map(|(_, n)| n))
        .unwrap_or_default()
}

/// Фоновые потоки «Игры» уступают процессор игре. Время ответа меряет ядро, приоритет на него не влияет.
fn lower_priority() {
    use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
    }
}

/// Осмотр процессов: кто из известных помех запущен и качает ли.
#[derive(Default)]
struct Scanner {
    /// прошлый счётчик ввода-вывода: pid → (имя, байты, когда)
    prev_io: HashMap<u32, (String, u64, Instant)>,
    /// установлен ли модуль клипов Discord и когда проверяли (папки читаем не чаще раза в 10 мин)
    clips: Option<(bool, Instant)>,
}

impl Scanner {
    fn run(&mut self) -> Vec<Culprit> {
        let now = Instant::now();
        let mut procs: Vec<Proc> = Vec::new();
        for (pid, name) in snapshot() {
            let exe = name.to_lowercase();
            if !is_watched(&exe) {
                continue;
            }
            let h = open_proc(pid);
            let path = h.as_ref().and_then(path_of);
            let mut rate = None;
            if io_watched(&exe) {
                if let Some(cur) = h.as_ref().and_then(io_of) {
                    let prev = self.prev_io.get(&pid).filter(|p| p.0 == exe).map(|p| (p.1, now.saturating_duration_since(p.2).as_secs_f64()));
                    rate = io_rate(prev.map(|p| p.0), cur, prev.map_or(0.0, |p| p.1));
                    // слишком короткий промежуток — копим дальше от старой точки
                    if prev.is_none_or(|p| p.1 >= 0.5) {
                        self.prev_io.insert(pid, (exe.clone(), cur, now));
                    }
                }
            }
            procs.push(Proc { pid, exe, path, io_rate: rate });
        }
        self.prev_io.retain(|pid, _| procs.iter().any(|p| p.pid == *pid));
        let running = |list: &[&str]| procs.iter().any(|p| list.contains(&p.exe.as_str()));
        let clips_module = running(DISCORD)
            && match self.clips {
                Some((v, at)) if now.saturating_duration_since(at) < Duration::from_secs(600) => v,
                _ => {
                    let v = clips_module_installed();
                    self.clips = Some((v, now));
                    v
                }
            };
        let flags = Flags { wu_service: service_running(), steam_updating: running(&["steam.exe"]) && steam_updating(), clips_module, xbox_bg: xbox_bg_record() };
        detect(&procs, &flags)
    }
}

/// Два фоновых потока: замер пинга и наблюдение за игрой/процессами. sessions = false — игры не ловим (тестовый запуск).
pub fn start(state: Arc<Mutex<State>>, ctx: eframe::egui::Context, sessions: bool) {
    {
        let (state, ctx) = (state.clone(), ctx.clone());
        std::thread::spawn(move || ping_loop(&state, &ctx));
    }
    std::thread::spawn(move || watch_loop(&state, &ctx, sessions));
}

/// Раз в секунду пинг до роутера и до интернета — только когда страница открыта или идёт игра.
fn ping_loop(state: &Mutex<State>, ctx: &eframe::egui::Context) {
    lower_priority();
    let mut icmp: Option<Icmp> = None;
    let (mut gw, mut gw_at, mut gw_fails): (Option<Ipv4Addr>, Option<Instant>, u32) = (None, None, 0);
    // адрес интернета: отвечал ли хоть раз, сколько потерь подряд, все ли уже перепробованы
    let (mut target, mut ever_ok, mut fails, mut tried_all) = (0usize, false, 0u32, false);
    let mut last: Option<Instant> = None;
    loop {
        let t0 = Instant::now();
        let (want, visible) = match state.lock() {
            Ok(s) => (ping_wanted(s.visible, s.session.is_some(), s.frozen), s.visible),
            Err(_) => return,
        };
        if !want {
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        // замер долго стоял: старые точки не рисуем, сеть могла смениться
        if should_reset(last.map(|l| t0.saturating_duration_since(l).as_secs_f64())) {
            match state.lock() {
                Ok(mut s) => {
                    s.gw_ring.clear();
                    s.inet_ring.clear();
                }
                Err(_) => return,
            }
            gw_at = None;
        }
        if icmp.is_none() {
            icmp = Icmp::open();
        }
        let Some(ic) = icmp.as_ref() else {
            match state.lock() {
                Ok(mut s) => s.icmp_failed = true,
                Err(_) => return,
            }
            if visible {
                ctx.request_repaint();
            }
            std::thread::sleep(Duration::from_secs(5));
            continue;
        };
        let old_gw = gw;
        if gw_at.is_none_or(|t| t0.saturating_duration_since(t) >= GW_REFRESH) || gw_fails >= 5 {
            gw = gateway();
            gw_at = Some(t0);
            gw_fails = 0;
            if gw != old_gw {
                // сеть сменилась: в новой 1.1.1.1 может и не отвечать — адрес интернета выбираем заново
                (target, ever_ok, fails, tried_all) = (0, false, 0, false);
            }
        }
        let g = gw.map(|ip| ic.echo(ip, GW_TIMEOUT_MS));
        gw_fails = if g == Some(None) { gw_fails + 1 } else { 0 };
        let r = ic.echo(INET_TARGETS[target], INET_TIMEOUT_MS);
        if r.is_some() {
            ever_ok = true;
            fails = 0;
        } else {
            fails = fails.saturating_add(1);
        }
        let next = next_target(target, ever_ok, fails);
        let switched = next != target;
        if switched {
            target = next;
            ever_ok = false;
            fails = 0;
        }
        last = Some(Instant::now());
        match state.lock() {
            Ok(mut s) => {
                s.icmp_failed = false;
                if gw != old_gw {
                    s.gw_ring.clear();
                }
                s.gw = gw;
                s.target = target;
                if let Some(v) = g {
                    push_ring(&mut s.gw_ring, v, RING);
                }
                if switched && !tried_all {
                    // адрес молчал с самого начала (провайдер режет ICMP) — его «потери» не показываем
                    while s.inet_ring.back() == Some(&None) {
                        s.inet_ring.pop_back();
                    }
                } else if ever_ok || r.is_some() {
                    push_ring(&mut s.inet_ring, r, RING);
                }
                // ни один адрес ни разу не ответил (сеть режет ICMP, интернет при этом работает) —
                // потери не пишем, иначе страница показывала красное «Плохо, потери 100%»
                // в сводку игры — только замеры адреса, который хоть раз ответил
                if ever_ok {
                    if let Some(l) = s.session.as_mut() {
                        l.push(r, g);
                    }
                }
            }
            Err(_) => return,
        }
        if switched && target == 0 {
            tried_all = true;
        }
        // в игре интерфейс не будим: страница закрыта
        if visible {
            ctx.request_repaint();
        }
        std::thread::sleep(Duration::from_secs(1).saturating_sub(t0.elapsed()));
    }
}

/// Раз в секунду: не началась ли игра, жива ли она; осмотр процессов — только когда страница открыта или идёт игра.
fn watch_loop(state: &Mutex<State>, ctx: &eframe::egui::Context, sessions: bool) {
    lower_priority();
    let mut tracker = Tracker::default();
    // хэндл игры держим открытым: пока он жив, её pid не достанется другому процессу
    let mut game: Option<Owned> = None;
    let mut scanner = Scanner::default();
    let mut last_scan: Option<Instant> = None;
    // прошлый тик наблюдения: большой разрыв — компьютер спал
    let mut prev_tick: Option<Instant> = None;
    loop {
        let (frozen, visible) = match state.lock() {
            Ok(s) => (s.frozen, s.visible),
            Err(_) => return,
        };
        if frozen {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        if sessions {
            // полноэкранность — общий флаг тихого режима (интерфейс ставит его по монитору острова):
            // второй раз Windows не опрашиваем. Замер пинга и этот поток в игре работают — для этого они и нужны.
            let fullscreen = crate::quiet::in_game();
            let fg_pid = if fullscreen { foreground_pid() } else { 0 };
            // имя нужно только чтобы начать игру: в игре сверяем лишь pid
            let fg_exe = if fg_pid != 0 && !tracker.active() { exe_name(fg_pid) } else { String::new() };
            let alive = match tracker.pid() {
                None => true,
                Some((pid, exe)) => match &game {
                    Some(h) => !exited(h),
                    // античит не дал открыть процесс — смотрим, есть ли он в списке (имя не узнали — хватит pid)
                    None => snapshot().iter().any(|(p, n)| *p == pid && (exe.is_empty() || n.to_lowercase() == exe)),
                },
            };
            let now = Instant::now();
            let step = match prev_tick {
                Some(p) if stalled(p, now) => tracker.interrupted(p),
                _ => tracker.tick(now, &Seen { fullscreen, fg_pid, fg_exe: &fg_exe, alive }),
            };
            prev_tick = Some(now);
            match step {
                Step::Started { pid, exe } => {
                    game = open_proc(pid);
                    // осмотреть сразу: что мешает, видно с первой минуты
                    last_scan = None;
                    match state.lock() {
                        Ok(mut s) => s.session = Some(Live::new(&exe, now)),
                        Err(_) => return,
                    }
                }
                Step::Ended { exe, secs } => {
                    game = None;
                    let done = match state.lock() {
                        Ok(mut s) => {
                            let live = s.session.take().unwrap_or_else(|| Live::new(&exe, now));
                            let worth = worth_summary(secs);
                            if worth {
                                let sum = summarize(&live, secs, chrono::Local::now().format("%H:%M").to_string());
                                s.last = Some(sum.clone());
                                s.events.push(sum);
                            }
                            worth
                        }
                        Err(_) => return,
                    };
                    if done {
                        ctx.request_repaint();
                    }
                }
                Step::Nothing => {}
            }
        }
        if let Some(every) = scan_every(visible, tracker.active()) {
            if last_scan.is_none_or(|t| t.elapsed() >= every) {
                last_scan = Some(Instant::now());
                let list = scanner.run();
                match state.lock() {
                    Ok(mut s) => {
                        if let Some(l) = s.session.as_mut() {
                            l.note(&list);
                        }
                        s.culprits = list;
                        s.scanned = true;
                    }
                    Err(_) => return,
                }
                if visible {
                    ctx.request_repaint();
                }
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Level::{Green, Red, Yellow};

    /// Замеры: отрицательное — потеря.
    fn v(xs: &[i64]) -> Vec<Option<u32>> {
        xs.iter().map(|&x| (x >= 0).then_some(x as u32)).collect()
    }

    fn st(median: u32, jitter: f32, sent: usize, lost: usize) -> Stats {
        Stats { sent, lost, avg: median as f32, min: median, max: median, median, jitter }
    }

    fn p(exe: &str) -> Proc {
        Proc { pid: 1, exe: exe.into(), path: None, io_rate: None }
    }

    fn pp(exe: &str, path: &str) -> Proc {
        Proc { pid: 2, exe: exe.into(), path: Some(path.into()), io_rate: None }
    }

    fn pio(exe: &str, rate: Option<f64>) -> Proc {
        Proc { pid: 3, exe: exe.into(), path: Some(format!(r"C:\Program Files\X\{exe}")), io_rate: rate }
    }

    fn kinds(c: &[Culprit]) -> Vec<Kind> {
        c.iter().map(|c| c.kind).collect()
    }

    fn seen(fullscreen: bool, fg_pid: u32, fg_exe: &str, alive: bool) -> Seen<'_> {
        Seen { fullscreen, fg_pid, fg_exe, alive }
    }

    fn sum(inet: Option<Stats>, place: Place, spikes: usize, worst: Option<u32>, blame: Option<Kind>, secs: u64) -> Summary {
        let level = inet.filter(|s| s.ok() > 0).map(|s| level_of(&s, &INET));
        Summary { title: "CS2".into(), secs, ended_at: "21:40".into(), inet, gw: None, level, place, spikes, worst, blame, hist: Vec::new() }
    }

    #[test]
    fn ring_caps_at_60() {
        let mut r = VecDeque::new();
        for i in 0..70u32 {
            push_ring(&mut r, Some(i), RING);
        }
        assert_eq!(r.len(), 60);
        assert_eq!(r[0], Some(10));
        assert_eq!(r[59], Some(69));
    }

    #[test]
    fn stats_basic() {
        let s = stats(&v(&[20, 30, -1, 10])).unwrap();
        assert_eq!((s.sent, s.lost, s.min, s.max, s.median), (4, 1, 10, 30, 20));
        assert!((s.avg - 20.0).abs() < 1e-4);
        assert!((s.jitter - 15.0).abs() < 1e-4);
        assert_eq!(s.ok(), 3);
    }

    #[test]
    fn stats_empty_is_none() {
        assert_eq!(stats(&[]), None);
    }

    #[test]
    fn stats_all_lost() {
        let s = stats(&v(&[-1, -1, -1])).unwrap();
        assert_eq!((s.sent, s.lost, s.median, s.min, s.max), (3, 3, 0, 0, 0));
        assert_eq!(s.avg, 0.0);
        assert_eq!(s.jitter, 0.0);
        assert_eq!(s.loss_pct(), 100.0);
        assert_eq!(s.ok(), 0);
    }

    #[test]
    fn median_even_count() {
        assert_eq!(stats(&v(&[40, 10, 30, 20])).unwrap().median, 25);
        assert_eq!(stats(&v(&[7])).unwrap().median, 7);
    }

    #[test]
    fn jitter_skips_lost() {
        assert!((stats(&v(&[10, -1, 14])).unwrap().jitter - 4.0).abs() < 1e-4);
        // один успешный замер — джиттер не считается
        assert_eq!(stats(&v(&[10, -1])).unwrap().jitter, 0.0);
    }

    #[test]
    fn loss_percent() {
        let mut xs = vec![20i64; 59];
        xs.push(-1);
        let s = stats(&v(&xs)).unwrap();
        assert!((s.loss_pct() - 1.6667).abs() < 0.01);
        assert_eq!(Stats::default().loss_pct(), 0.0, "без замеров не делим на ноль");
    }

    #[test]
    fn inet_levels() {
        let pl = |m| ping_level(&st(m, 0.0, 10, 0), &INET);
        assert_eq!([pl(60), pl(61), pl(100), pl(101)], [Green, Yellow, Yellow, Red]);
        let jl = |j| jitter_level(&st(20, j, 10, 0), &INET);
        assert_eq!([jl(5.0), jl(5.1), jl(15.0), jl(15.1)], [Green, Yellow, Yellow, Red]);
        let ll = |lost| loss_level(&st(20, 0.0, 1000, lost), &INET);
        assert_eq!([ll(0), ll(17), ll(20), ll(21)], [Green, Yellow, Yellow, Red]);
        assert_eq!([ll(5), ll(6)], [Green, Yellow], "до 0,5% потерь — ещё зелёный");
        assert_eq!(loss_level(&st(20, 0.0, 60, 1), &INET), Yellow, "на странице одна потеря из 60 — уже жёлтый");
        // уровень — худший из трёх
        assert_eq!(level_of(&st(20, 1.0, 1000, 0), &INET), Green);
        assert_eq!(level_of(&st(80, 1.0, 1000, 0), &INET), Yellow);
        assert_eq!(level_of(&st(20, 20.0, 1000, 0), &INET), Red);
        assert_eq!(level_of(&st(20, 1.0, 1000, 30), &INET), Red);
        assert_eq!(level_of(&st(80, 10.0, 1000, 5), &INET), Yellow);
    }

    #[test]
    fn gateway_levels() {
        let pl = |m| level_of(&st(m, 0.0, 10, 0), &GATEWAY);
        assert_eq!([pl(10), pl(11), pl(30), pl(31)], [Green, Yellow, Yellow, Red]);
    }

    #[test]
    fn page_level_waits_for_samples() {
        assert_eq!(page_level(None), None);
        assert_eq!(page_level(Some(&st(20, 1.0, 2, 0))), None, "двух замеров мало");
        assert_eq!(page_level(Some(&st(20, 1.0, 3, 0))), Some(Green));
        assert_eq!(page_level(Some(&Stats { sent: 10, lost: 10, ..Default::default() })), Some(Red), "интернет не отвечает — плохо");
    }

    #[test]
    fn place_rules() {
        let (gi, yi, ri) = (st(20, 1.0, 60, 0), st(80, 1.0, 60, 0), st(150, 1.0, 60, 0));
        let (gg, yg, rg) = (st(2, 1.0, 60, 0), st(20, 1.0, 60, 0), st(50, 1.0, 60, 0));
        assert_eq!(place(Some(&gg), &yi), Place::Beyond);
        assert_eq!(place(Some(&yg), &yi), Place::Beyond, "20 из 80 мс до роутера: основная задержка дальше");
        assert_eq!(place(Some(&st(45, 1.0, 60, 0)), &yi), Place::Home, "больше половины пинга — уже до роутера");
        assert_eq!(place(Some(&yg), &ri), Place::Beyond);
        assert_eq!(place(Some(&rg), &ri), Place::Beyond, "50 из 150 мс — больше за роутером");
        assert_eq!(place(Some(&st(90, 1.0, 60, 0)), &ri), Place::Home);
        assert_eq!(place(Some(&rg), &gi), Place::Fine);
        assert_eq!(place(None, &ri), Place::Unknown);
        assert_eq!(place(None, &gi), Place::Fine);
        // ни роутер, ни интернет не отвечают — дома (Wi‑Fi отвалился)
        let dead = Stats { sent: 60, lost: 60, ..Default::default() };
        assert_eq!(place(Some(&dead), &dead), Place::Home);
    }

    #[test]
    fn router_own_losses_do_not_mean_home() {
        // роутер отвечает на пинг к себе в последнюю очередь: его потери без потерь до интернета ничего не значат
        let gw = st(2, 1.0, 100, 10);
        assert_eq!(place(Some(&gw), &st(30, 9.0, 100, 0)), Place::Beyond, "интернет плох джиттером, роутер только теряет пинг к себе");
        assert_eq!(place(Some(&gw), &st(80, 1.0, 100, 0)), Place::Beyond);
        // потери и там и там — дома (Wi‑Fi)
        assert_eq!(place(Some(&st(3, 1.0, 100, 4)), &st(30, 1.0, 100, 5)), Place::Home);
        // до интернета теряется намного больше, чем до роутера, — дальше
        assert_eq!(place(Some(&st(3, 1.0, 100, 1)), &st(30, 1.0, 100, 8)), Place::Beyond);
    }

    #[test]
    fn wifi_jitter_is_home() {
        assert_eq!(place(Some(&st(6, 9.0, 60, 0)), &st(30, 12.0, 60, 0)), Place::Home);
        assert_eq!(place(Some(&st(2, 5.0, 60, 0)), &st(30, 14.0, 60, 0)), Place::Beyond, "дома скачет втрое меньше");
        assert_eq!(place(Some(&st(2, 3.0, 60, 0)), &st(30, 5.5, 60, 0)), Place::Beyond, "до роутера джиттер в норме");
        // плох прежде всего пинг (далёкий сервер), а джиттер дома лишь «жёлтый» — винить дом не за что
        assert_eq!(place(Some(&st(6, 9.0, 60, 0)), &st(150, 12.0, 60, 0)), Place::Beyond);
    }

    #[test]
    fn silent_gateway_is_unknown() {
        let gw = Stats { sent: 60, lost: 60, ..Default::default() };
        let inet = st(80, 1.0, 60, 0);
        assert!(!usable_gw(&gw, &inet));
        assert!(usable_gw(&st(2, 0.0, 60, 0), &inet));
        assert_eq!(place(Some(&gw), &inet), Place::Unknown);
    }

    #[test]
    fn spikes_detected() {
        assert_eq!(spikes(&v(&[20; 30])), (0, None));
        let mut one = vec![20i64; 30];
        one[10] = 300;
        assert_eq!(spikes(&v(&one)), (1, Some(300)));
        let mut run = vec![20i64; 30];
        run[10] = 200;
        run[11] = 250;
        assert_eq!(spikes(&v(&run)), (1, Some(250)));
        // потеря внутри скачка его не разрывает
        let mut gap = vec![20i64; 30];
        gap[10] = 200;
        gap[11] = -1;
        gap[12] = 220;
        assert_eq!(spikes(&v(&gap)), (1, Some(220)));
        let mut two = vec![20i64; 30];
        two[5] = 200;
        two[20] = 180;
        assert_eq!(spikes(&v(&two)), (2, Some(200)));
        let mut small = vec![20i64; 30];
        small[3] = 60;
        assert_eq!(spikes(&v(&small)), (0, None), "+40 мс — не скачок");
        let mut base = vec![100i64; 30];
        base[3] = 199;
        assert_eq!(spikes(&v(&base)), (0, None));
        base[20] = 200;
        assert_eq!(spikes(&v(&base)), (1, Some(200)));
        assert_eq!(spikes(&v(&[-1, -1])), (0, None));
        assert_eq!(spikes(&[]), (0, None));
    }

    #[test]
    fn graph_scale_steps() {
        assert_eq!(graph_scale(10), 50);
        assert_eq!(graph_scale(40), 50);
        assert_eq!(graph_scale(45), 100);
        assert_eq!(graph_scale(300), 500);
        assert_eq!(graph_scale(5000), 1000);
        assert_eq!(graph_scale(0), 50);
    }

    #[test]
    fn downsample_keeps_max() {
        let xs: Vec<Option<u32>> = (0..120u32).map(Some).collect();
        let d = downsample_max(&xs, 60);
        assert_eq!(d.len(), 60);
        assert_eq!(d[0], Some(1));
        assert_eq!(d[59], Some(119));
        let mut holes = xs.clone();
        holes[0] = None;
        holes[1] = None;
        holes[2] = None;
        let d = downsample_max(&holes, 60);
        assert_eq!(d[0], None, "корзина из одних потерь");
        assert_eq!(d[1], Some(3));
        let short = v(&[5, -1, 7]);
        assert_eq!(downsample_max(&short, 60), short);
        // длина, не кратная числу корзин, — всё равно ровно n точек и ничего не теряется
        let odd: Vec<Option<u32>> = (0..61u32).map(Some).collect();
        let d = downsample_max(&odd, 60);
        assert_eq!(d.len(), 60);
        assert_eq!(d[59], Some(60));
        assert!(downsample_max(&odd, 0).is_empty());
    }

    #[test]
    fn ip_raw_round_trip() {
        for ip in [Ipv4Addr::new(192, 168, 1, 1), Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(10, 0, 0, 254)] {
            assert_eq!(raw_to_ip(ip_to_raw(ip)), ip);
        }
        assert_eq!(ip_to_raw(Ipv4Addr::new(192, 168, 1, 1)).to_ne_bytes(), [192, 168, 1, 1]);
    }

    #[test]
    fn fmt_helpers() {
        assert_eq!(fmt_pct(0.0), "0%");
        assert_eq!(fmt_pct(1.666), "1,7%");
        assert_eq!(fmt_pct(4.0), "4%");
        assert_eq!(fmt_pct(12.3), "12%");
        assert_eq!(fmt_pct(100.0), "100%");
        assert_eq!(fmt_pct(0.01), "<0,1%", "крошечные потери — не «0%»");
        assert_eq!(fmt_pct(f32::NAN), "0%");
        assert_eq!(fmt_rate(655_360.0), "640 КБ/с");
        assert_eq!(fmt_rate(3.2 * 1_048_576.0), "3,2 МБ/с");
        assert_eq!(fmt_ms(None), "—");
        assert_eq!(fmt_ms(Some(0)), "<1 мс");
        assert_eq!(fmt_ms(Some(22)), "22 мс");
    }

    #[test]
    fn colors_and_words() {
        assert_eq!(level_rgb(Some(Green)), [0x7B, 0xE0, 0x9A]);
        assert_eq!(level_rgb(Some(Yellow)), [0xFF, 0xB3, 0x40]);
        assert_eq!(level_rgb(Some(Red)), [0xFF, 0x6B, 0x6B]);
        assert_eq!(level_rgb(None), [0xC4, 0xC8, 0xD4]);
        assert_eq!(level_word(Some(Green)), "Отлично");
        assert_eq!(level_word(None), "Собираю данные…");
        assert_eq!(place_lines(Place::Home).0, "проблема дома");
        assert_eq!(place_lines(Place::Fine), ("сеть в порядке", "можно играть"));
        // подписи понятны без слов «шлюз» и «ICMP»
        for pl in [Place::Fine, Place::Home, Place::Beyond, Place::Unknown] {
            let (a, b) = place_lines(pl);
            assert!(!a.contains("шлюз") && !b.contains("ICMP"));
            assert!(b.chars().count() <= 30, "вторая строка влезает в карточку: {b}");
        }
    }

    #[test]
    fn target_fallback() {
        assert_eq!(next_target(0, false, 5), 1);
        assert_eq!(next_target(0, true, 9), 0);
        assert_eq!(next_target(0, false, 4), 0);
        assert_eq!(next_target(1, false, 5), 0, "по кругу");
    }

    #[test]
    fn stale_rings_reset() {
        assert!(!should_reset(None));
        assert!(!should_reset(Some(3.0)));
        assert!(should_reset(Some(6.0)));
    }

    #[test]
    fn poll_schedule() {
        assert!(!ping_wanted(false, false, false), "вне игры с закрытой страницей — ни одного пинга");
        assert!(ping_wanted(true, false, false));
        assert!(ping_wanted(false, true, false));
        assert!(ping_wanted(true, true, false));
        assert!(!ping_wanted(true, true, true), "frozen всё глушит");
        assert!(!ping_wanted(false, true, true));
        assert_eq!(scan_every(true, false), Some(SCAN_VISIBLE));
        assert_eq!(scan_every(true, true), Some(SCAN_VISIBLE));
        assert_eq!(scan_every(false, true), Some(SCAN_GAME));
        assert_eq!(scan_every(false, false), None);
    }

    #[test]
    fn browsers_and_players_are_not_games() {
        for exe in ["chrome.exe", "msedge.exe", "firefox.exe", "browser.exe", "vlc.exe", "potplayermini64.exe", "applicationframehost.exe", "explorer.exe", "obs64.exe"] {
            assert!(not_a_game(exe), "{exe}");
        }
        assert!(not_a_game(&"Chrome.EXE".to_lowercase()));
        assert!(not_a_game("Chrome.EXE"), "регистр не важен");
        assert!(!not_a_game("cs2.exe"));
        assert!(!not_a_game(""), "путь не прочитался — может быть игра под античитом");
        assert!(!not_a_game("chromeplus_game.exe"), "сравнение по имени целиком");
    }

    #[test]
    fn game_titles() {
        assert_eq!(game_title("cs2.exe"), "CS2");
        assert_eq!(game_title("CS2.EXE"), "CS2");
        assert_eq!(game_title("r5apex.exe"), "Apex Legends");
        assert_eq!(game_title("SomeGame.exe"), "SomeGame");
        assert_eq!(game_title("somegame.Exe"), "Somegame");
        assert_eq!(game_title(""), "Игра");
        assert_eq!(game_title(".exe"), "Игра");
    }

    #[test]
    fn session_starts_on_fullscreen_game() {
        let t0 = Instant::now();
        let mut tr = Tracker::default();
        assert!(!tr.active());
        assert_eq!(tr.tick(t0, &seen(true, 42, "CS2.exe", true)), Step::Started { pid: 42, exe: "CS2.exe".into() });
        assert!(tr.active());
        assert_eq!(tr.pid(), Some((42, "cs2.exe")));
        assert_eq!(tr.tick(t0 + Duration::from_secs(1), &seen(true, 42, "cs2.exe", true)), Step::Nothing);
    }

    #[test]
    fn browser_fullscreen_starts_nothing() {
        let t0 = Instant::now();
        let mut tr = Tracker::default();
        assert_eq!(tr.tick(t0, &seen(true, 7, "chrome.exe", true)), Step::Nothing);
        assert_eq!(tr.tick(t0, &seen(false, 42, "cs2.exe", true)), Step::Nothing, "не на весь экран");
        assert_eq!(tr.tick(t0, &seen(true, 0, "cs2.exe", true)), Step::Nothing, "окна нет");
        assert!(!tr.active());
        assert_eq!(tr.pid(), None);
    }

    #[test]
    fn short_alt_tab_keeps_session() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut tr = Tracker::default();
        tr.tick(at(0), &seen(true, 42, "cs2.exe", true));
        assert_eq!(tr.tick(at(100), &seen(false, 0, "", true)), Step::Nothing);
        assert_eq!(tr.tick(at(130), &seen(false, 0, "", true)), Step::Nothing);
        assert_eq!(tr.tick(at(131), &seen(true, 42, "cs2.exe", true)), Step::Nothing);
        // вернулись — счёт времени вне игры начинается заново
        assert_eq!(tr.tick(at(300), &seen(false, 0, "", true)), Step::Nothing);
        assert_eq!(tr.tick(at(410), &seen(true, 42, "cs2.exe", true)), Step::Nothing);
        assert_eq!(tr.tick(at(600), &seen(true, 42, "cs2.exe", false)), Step::Ended { exe: "cs2.exe".into(), secs: 600 });
        assert!(!tr.active());
    }

    #[test]
    fn long_away_ends_session() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut tr = Tracker::default();
        tr.tick(at(0), &seen(true, 42, "cs2.exe", true));
        assert_eq!(tr.tick(at(1000), &seen(false, 0, "", true)), Step::Nothing);
        assert_eq!(tr.tick(at(1119), &seen(false, 0, "", true)), Step::Nothing);
        assert_eq!(tr.tick(at(1121), &seen(false, 0, "", true)), Step::Ended { exe: "cs2.exe".into(), secs: 1000 });
        assert!(!tr.active());
    }

    #[test]
    fn process_exit_ends_immediately() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut tr = Tracker::default();
        tr.tick(at(0), &seen(true, 42, "cs2.exe", true));
        assert_eq!(tr.tick(at(300), &seen(true, 42, "cs2.exe", false)), Step::Ended { exe: "cs2.exe".into(), secs: 300 });
        // закрыли, пока были вне игры: считаем до ухода
        tr.tick(at(400), &seen(true, 42, "cs2.exe", true));
        tr.tick(at(600), &seen(false, 0, "", true));
        assert_eq!(tr.tick(at(650), &seen(false, 0, "", false)), Step::Ended { exe: "cs2.exe".into(), secs: 200 });
    }

    #[test]
    fn other_fullscreen_app_counts_as_away() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut tr = Tracker::default();
        tr.tick(at(0), &seen(true, 42, "cs2.exe", true));
        assert_eq!(tr.tick(at(10), &seen(true, 99, "vlc.exe", true)), Step::Nothing);
        assert_eq!(tr.tick(at(130), &seen(true, 99, "vlc.exe", true)), Step::Ended { exe: "cs2.exe".into(), secs: 10 });
    }

    #[test]
    fn sleep_interrupts_session() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert!(!stalled(at(10), at(11)));
        assert!(!stalled(at(10), at(129)));
        assert!(stalled(at(10), at(130)));
        assert!(!stalled(at(20), at(10)), "часы назад — не сон");
        let mut tr = Tracker::default();
        assert_eq!(tr.interrupted(at(0)), Step::Nothing, "игры нет — нечего обрывать");
        tr.tick(at(0), &seen(true, 42, "cs2.exe", true));
        tr.tick(at(600), &seen(true, 42, "cs2.exe", true));
        // уснул посреди игры: сон в длительность не идёт
        assert_eq!(tr.interrupted(at(600)), Step::Ended { exe: "cs2.exe".into(), secs: 600 });
        assert!(!tr.active());
        // после сна игра на экране — новая сессия
        assert!(matches!(tr.tick(at(9000), &seen(true, 42, "cs2.exe", true)), Step::Started { .. }));
        // уснул, уже выйдя из игры: считаем до ухода
        tr.tick(at(9300), &seen(false, 0, "", true));
        assert_eq!(tr.interrupted(at(9310)), Step::Ended { exe: "cs2.exe".into(), secs: 300 });
    }

    #[test]
    fn unknown_exe_counts_as_game() {
        let mut tr = Tracker::default();
        assert_eq!(tr.tick(Instant::now(), &seen(true, 5, "", true)), Step::Started { pid: 5, exe: String::new() });
        assert_eq!(tr.pid(), Some((5, "")));
    }

    #[test]
    fn worth_summary_needs_three_minutes() {
        assert!(!worth_summary(179));
        assert!(worth_summary(180));
    }

    #[test]
    fn live_collects_samples() {
        let mut l = Live::new("cs2.exe", Instant::now());
        assert_eq!(l.title, "CS2");
        l.push(Some(20), Some(Some(2)));
        l.push(None, None);
        assert_eq!(l.inet, VecDeque::from(vec![Some(20), None]));
        assert_eq!(l.gw, VecDeque::from(vec![Some(2)]), "шлюз неизвестен — замер роутера не пишем");
        let c = |kind| Culprit { kind, title: String::new(), detail: String::new(), open: None, sure: true };
        l.note(&[c(Kind::Build), c(Kind::Updates)]);
        l.note(&[c(Kind::Updates)]);
        assert_eq!(l.seen, vec![Kind::Build, Kind::Updates]);
        l.note(&[Culprit { sure: false, ..c(Kind::Steam) }]);
        assert_eq!(l.seen, vec![Kind::Build, Kind::Updates], "косвенный признак за игру не копим");
        for _ in 0..MAX_SESSION_SAMPLES + 10 {
            l.push(Some(1), None);
        }
        assert_eq!(l.inet.len(), MAX_SESSION_SAMPLES);
    }

    #[test]
    fn io_rate_cases() {
        assert_eq!(io_rate(None, 5, 2.0), None);
        assert_eq!(io_rate(Some(0), 5, 0.2), None);
        assert_eq!(io_rate(Some(10), 5, 2.0), None);
        assert_eq!(io_rate(Some(0), 2_000_000, 2.0), Some(1e6));
        assert_eq!(io_rate(Some(0), 40_000_000, 20.0), Some(2e6), "раз в 20 с в игре — считаем");
        assert_eq!(io_rate(Some(0), 600_000_000, 600.0), None, "замеру десять минут — среднее ничего не говорит");
        assert_eq!(io_rate(Some(0), 5, f64::NAN), None);
    }

    #[test]
    fn watched_tables() {
        assert!(is_watched("qbittorrent.exe"));
        assert!(is_watched("steam.exe"));
        assert!(is_watched("onedrive.exe"));
        assert!(is_watched("nvidia overlay.exe"));
        assert!(is_watched("tiworker.exe"));
        assert!(!is_watched("cs2.exe"));
        assert!(io_watched("onedrive.exe"));
        assert!(io_watched("epicgameslauncher.exe"));
        assert!(!io_watched("qbittorrent.exe"));
    }

    #[test]
    fn detect_clean_system_is_empty() {
        assert!(detect(&[p("explorer.exe"), p("chrome.exe"), p("cs2.exe")], &Flags::default()).is_empty());
        assert!(detect(&[], &Flags::default()).is_empty());
    }

    #[test]
    fn detect_windows_update() {
        let c = detect(&[p("tiworker.exe")], &Flags::default());
        assert_eq!(kinds(&c), vec![Kind::Updates]);
        assert_eq!(c[0].title, "Устанавливаются обновления Windows");
        assert!(c[0].detail.contains("TiWorker"), "{}", c[0].detail);
        assert_eq!(c[0].open, Some(Open::Uri("ms-settings:windowsupdate")));
        assert!(c[0].sure, "идёт установка — явная помеха");
        let c = detect(&[], &Flags { wu_service: true, ..Default::default() });
        assert_eq!(c[0].title, "Обновления Windows могут качаться");
        assert!(c[0].detail.contains("wuauserv"));
        assert!(!c[0].sure, "одна служба — только косвенный признак");
        // и процесс, и служба — один пункт
        let c = detect(&[p("tiworker.exe"), p("tiworker.exe")], &Flags { wu_service: true, ..Default::default() });
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].detail, "TiWorker, служба wuauserv");
    }

    #[test]
    fn detect_torrent_buttons() {
        let c = detect(&[pp("qbittorrent.exe", r"C:\Program Files\qBittorrent\qbittorrent.exe")], &Flags::default());
        assert_eq!(c[0].kind, Kind::Torrent);
        assert_eq!(c[0].title, "Торрент-клиент qBittorrent");
        assert_eq!(c[0].open, Some(Open::Exe(r"C:\Program Files\qBittorrent\qbittorrent.exe".into())));
        let c = detect(&[pp("transmission-daemon.exe", r"C:\T\transmission-daemon.exe")], &Flags::default());
        assert_eq!(c[0].open, None, "у службы нет окна");
        let c = detect(&[p("qbittorrent.exe")], &Flags::default());
        assert_eq!(c[0].open, None, "путь не прочитался — кнопки нет");
        // два клиента — один пункт, первый по таблице
        let c = detect(&[p("utorrent.exe"), p("qbittorrent.exe")], &Flags::default());
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].title, "Торрент-клиент qBittorrent");
    }

    #[test]
    fn onedrive_needs_io() {
        assert!(detect(&[pio("onedrive.exe", None)], &Flags::default()).is_empty());
        assert!(detect(&[pio("onedrive.exe", Some(10_240.0))], &Flags::default()).is_empty());
        let c = detect(&[pio("onedrive.exe", Some(2.0 * 1_048_576.0))], &Flags::default());
        assert_eq!(kinds(&c), vec![Kind::OneDrive]);
        assert_eq!(c[0].detail, "2,0 МБ/с");
        assert!(matches!(c[0].open, Some(Open::Exe(_))));
    }

    #[test]
    fn steam_by_registry_or_io() {
        let flag = Flags { steam_updating: true, ..Default::default() };
        assert!(detect(&[], &flag).is_empty(), "флаг без steam.exe — нет");
        let c = detect(&[pio("steam.exe", None)], &flag);
        assert_eq!(c[0].title, "Steam: обновление в очереди", "флаг без скорости — не «качает»");
        assert_eq!(c[0].detail, "может начать качать прямо в игре");
        assert_eq!(c[0].open, Some(Open::Uri("steam://open/downloads")));
        assert!(!c[0].sure);
        let c = detect(&[pio("steam.exe", Some(3.0 * 1_048_576.0))], &Flags::default());
        assert_eq!(c[0].title, "Steam качает обновление");
        assert_eq!(c[0].detail, "3,0 МБ/с");
        assert!(c[0].sure);
        assert!(detect(&[pio("steam.exe", Some(500_000.0))], &Flags::default()).is_empty());
    }

    #[test]
    fn launcher_by_io() {
        assert!(detect(&[pio("battle.net.exe", Some(10_000.0))], &Flags::default()).is_empty());
        let c = detect(&[pio("epicgameslauncher.exe", Some(5.0 * 1_048_576.0))], &Flags::default());
        assert_eq!(kinds(&c), vec![Kind::Launcher]);
        assert_eq!(c[0].title, "Epic Games качает");
        assert_eq!(c[0].detail, "5,0 МБ/с");
    }

    #[test]
    fn discord_clips_needs_module_and_process() {
        let module = Flags { clips_module: true, ..Default::default() };
        assert!(detect(&[], &module).is_empty());
        assert!(detect(&[pp("discord.exe", r"C:\D\Discord.exe")], &Flags::default()).is_empty());
        let c = detect(&[pp("discord.exe", r"C:\D\Discord.exe")], &module);
        assert_eq!(kinds(&c), vec![Kind::DiscordClips]);
        assert_eq!(c[0].open, Some(Open::Exe(r"C:\D\Discord.exe".into())));
    }

    #[test]
    fn nvidia_overlay_detected() {
        let c = detect(&[p("nvidia overlay.exe")], &Flags::default());
        assert_eq!(kinds(&c), vec![Kind::NvidiaReplay]);
        assert_eq!(c[0].open, None);
        let c = detect(&[p("nvidia overlay.exe"), pp("nvidia app.exe", r"C:\N\NVIDIA App.exe")], &Flags::default());
        assert_eq!(c[0].open, Some(Open::Exe(r"C:\N\NVIDIA App.exe".into())));
        assert!(detect(&[pp("nvidia app.exe", r"C:\N\NVIDIA App.exe")], &Flags::default()).is_empty(), "само приложение без оверлея — не помеха");
    }

    #[test]
    fn xbox_bg_logic() {
        assert!(!xbox_bg(Some(0), Some(1), Some(1)));
        assert!(xbox_bg(None, Some(1), Some(1)));
        assert!(xbox_bg(Some(1), Some(1), Some(1)));
        assert!(!xbox_bg(Some(1), Some(1), None));
        assert!(!xbox_bg(Some(1), Some(0), Some(1)));
        let c = detect(&[], &Flags { xbox_bg: true, ..Default::default() });
        assert_eq!(kinds(&c), vec![Kind::XboxDvr]);
        assert_eq!(c[0].open, Some(Open::Uri("ms-settings:gaming-gamedvr")));
    }

    #[test]
    fn build_tools_listed_once() {
        let c = detect(&[p("cargo.exe"), p("rustc.exe"), p("rustc.exe")], &Flags::default());
        assert_eq!(kinds(&c), vec![Kind::Build]);
        assert_eq!(c[0].detail, "Cargo, Rustc");
        let many = detect(&[p("cargo.exe"), p("rustc.exe"), p("link.exe"), p("cl.exe")], &Flags::default());
        assert_eq!(many[0].detail, "Cargo, Rustc, Link", "не больше трёх имён");
    }

    #[test]
    fn culprits_sorted_by_priority() {
        let procs = [
            p("cl.exe"),
            pp("discord.exe", r"C:\D\Discord.exe"),
            p("nvidia share.exe"),
            pio("onedrive.exe", Some(5e6)),
            pio("eadesktop.exe", Some(5e6)),
            pio("steam.exe", Some(5e6)),
            pp("biglybt.exe", r"C:\B\BiglyBT.exe"),
            p("wuauclt.exe"),
        ];
        let all = Flags { wu_service: true, steam_updating: true, clips_module: true, xbox_bg: true };
        let c = detect(&procs, &all);
        use Kind::*;
        assert_eq!(kinds(&c), vec![Updates, Torrent, Steam, Launcher, OneDrive, DiscordClips, NvidiaReplay, XboxDvr, Build]);
    }

    #[test]
    fn open_targets_are_safe() {
        let procs = [p("tiworker.exe"), pio("steam.exe", Some(5e6)), p("cargo.exe"), pio("onedrive.exe", Some(5e6))];
        let all = Flags { wu_service: true, steam_updating: true, clips_module: true, xbox_bg: true };
        let mut opens: Vec<Open> = detect(&procs, &all).into_iter().filter_map(|c| c.open).collect();
        opens.extend(rows(&[], 5.0, 500.0).into_iter().filter_map(|r| r.open));
        assert!(opens.len() >= 5);
        for o in &opens {
            match o {
                Open::Uri(u) => assert!(ALLOWED_URIS.contains(u), "{u}"),
                Open::Exe(p) => assert!(safe_exe(p), "{p}"),
            }
        }
        assert!(safe_exe(r"C:\Program Files\qBittorrent\qbittorrent.exe"));
        assert!(safe_exe(r"D:\Games\Steam\STEAM.EXE"));
        assert!(!safe_exe(r"\\srv\a.exe"));
        assert!(!safe_exe("qbittorrent.exe"));
        assert!(!safe_exe(r"C:\x\a.bat"));
        assert!(!safe_exe(r"C:\x\a.exe.lnk"));
        assert!(!safe_exe(r#"C:\x\"a".exe"#));
        assert!(!safe_exe(""));
        // поток NTFS внутри другого файла, переменные окружения, длинный путь и путь от текущей папки диска
        assert!(!safe_exe(r"C:\x\notes.txt:evil.exe"));
        assert!(!safe_exe(r"C:\%TEMP%\a.exe"));
        assert!(!safe_exe(r"\\?\C:\x\a.exe"));
        assert!(!safe_exe(r"C:x\aaaa.exe"));
    }

    #[test]
    fn launch_targets_are_fixed() {
        // диспетчер задач — только полным путём из системной папки, а не по имени (иначе ищется в текущей папке)
        let t = launch_target("taskmgr").expect("системная папка известна");
        assert!(safe_exe(&t), "{t}");
        assert!(t.to_ascii_lowercase().ends_with(r"\system32\taskmgr.exe"), "{t}");
        assert_eq!(taskmgr_path(r"C:\Windows\System32\"), r"C:\Windows\System32\Taskmgr.exe");
        assert_eq!(taskmgr_path(r"C:\Windows\System32"), r"C:\Windows\System32\Taskmgr.exe");
        for u in ALLOWED_URIS.into_iter().filter(|u| *u != "taskmgr") {
            assert_eq!(launch_target(u).as_deref(), Some(u));
        }
        for bad in ["cmd", "calc.exe", "taskmgr.exe", r"C:\Windows\System32\cmd.exe", "ms-settings:", "", "steam://run/570"] {
            assert_eq!(launch_target(bad), None, "{bad}");
        }
    }

    #[test]
    fn open_labels_and_tips() {
        assert_eq!(open_label(&Open::Uri("ms-settings:windowsupdate")), "Параметры");
        assert_eq!(open_label(&Open::Uri("steam://open/downloads")), "Загрузки");
        assert_eq!(open_label(&Open::Uri("taskmgr")), "Диспетчер");
        assert_eq!(open_label(&Open::Exe(r"C:\a.exe".into())), "Открыть");
        assert!(open_tip(&Open::Exe(r"C:\Program Files\qBittorrent\qbittorrent.exe".into())).contains("qbittorrent.exe"));
        for u in ALLOWED_URIS {
            assert!(!open_tip(&Open::Uri(u)).is_empty());
        }
    }

    #[test]
    fn blame_prefers_network() {
        assert_eq!(blame(&[Kind::Build, Kind::OneDrive, Kind::Updates]), Some(Kind::Updates));
        assert_eq!(blame(&[Kind::Build, Kind::DiscordClips]), None);
        assert_eq!(blame(&[]), None);
        assert!(hits_network(Kind::Torrent) && !hits_network(Kind::XboxDvr));
    }

    #[test]
    fn service_alone_is_not_blamed() {
        let wu = Flags { wu_service: true, ..Default::default() };
        let list = detect(&[], &wu);
        assert_eq!(suspect(&list), None, "служба запускается сама и работает часто — не повод винить обновления");
        let mut l = Live::new("cs2.exe", Instant::now());
        l.note(&list);
        for i in 0..60 {
            // пинг скачет 20 ↔ 40 мс: джиттер 20, сеть плохая
            l.push(Some(if i % 2 == 0 { 20 } else { 40 }), Some(Some(2)));
        }
        let s = summarize(&l, 600, "22:00".into());
        assert_eq!(s.level, Some(Red));
        assert_eq!(s.blame, None);
        assert!(!summary_toast(&s).1.contains("Windows Update"), "{}", summary_toast(&s).1);
        assert_eq!(summary_toast(&s).1, "в интернете: провайдер или канал забит");
        // процесс установки — уже улика
        let list = detect(&[p("tiworker.exe")], &wu);
        assert_eq!(suspect(&list), Some(Kind::Updates));
        // Steam только с флагом в реестре — тоже не улика, со скоростью — улика
        let flag = Flags { steam_updating: true, ..Default::default() };
        assert_eq!(suspect(&detect(&[pio("steam.exe", None)], &flag)), None);
        assert_eq!(suspect(&detect(&[pio("steam.exe", Some(5e6))], &flag)), Some(Kind::Steam));
        // не сетевые помехи на сеть не наговаривают
        assert_eq!(suspect(&detect(&[p("cargo.exe")], &Flags { xbox_bg: true, ..Default::default() })), None);
    }

    #[test]
    fn rare_loss_in_long_game_is_fine() {
        let mut l = Live::new("cs2.exe", Instant::now());
        for i in 0..3600 {
            l.push(if i == 1800 { None } else { Some(22) }, Some(Some(2)));
        }
        let s = summarize(&l, 3600, "22:00".into());
        assert_eq!(s.level, Some(Green), "одна потеря за час игры — сеть в порядке");
        assert_eq!(summary_toast(&s).0, "Игра: пинг 22 мс, джиттер 0, потери <0,1%");
    }

    #[test]
    fn has_clips_module_cases() {
        assert!(has_clips_module(&["discord_voice-1".into(), "discord_clips-1".into()]));
        assert!(!has_clips_module(&["discord_voice-1".into()]));
        assert!(!has_clips_module(&[]));
    }

    #[test]
    fn low_disk_cases() {
        assert!(!low_disk(0.0, 0.0), "до первого замера — без ложной тревоги");
        assert!(low_disk(19.9, 500.0));
        assert!(!low_disk(20.0, 500.0));
        assert!(!low_disk(-1.0, 500.0));
    }

    #[test]
    fn rows_add_disk_after_culprits() {
        let c = detect(&[p("tiworker.exe"), p("cargo.exe")], &Flags::default());
        let r = rows(&c, 12.4, 500.0);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0].title, "Устанавливаются обновления Windows");
        assert!(r[0].network && !r[1].network);
        assert_eq!(r[0].hint, kind_hint(Kind::Updates));
        assert_eq!(r[2].title, "Мало места на диске C");
        assert_eq!(r[2].detail, "свободно 12 ГБ — игре и обновлениям нужно место");
        assert_eq!(r[2].open, Some(Open::Uri("ms-settings:storagesense")));
        assert_eq!(rows(&c, 200.0, 500.0).len(), c.len() + low_disk(200.0, 500.0) as usize);
    }

    #[test]
    fn kind_texts_present() {
        use Kind::*;
        for k in [Updates, Torrent, Steam, Launcher, OneDrive, DiscordClips, NvidiaReplay, XboxDvr, Build] {
            assert!(!kind_glyph(k).is_empty() && !kind_short(k).is_empty() && !kind_hint(k).is_empty());
        }
        assert_eq!(kind_short(Updates), "Windows Update");
        assert_eq!(kind_glyph(Steam), "\u{E7FC}");
    }

    #[test]
    fn summarize_builds_from_samples() {
        let mut l = Live::new("cs2.exe", Instant::now());
        for i in 0..60 {
            l.push(Some(if i == 30 { 300 } else { 20 }), Some(Some(2)));
        }
        l.seen = vec![Kind::Build, Kind::Updates];
        let s = summarize(&l, 4320, "21:40".into());
        assert_eq!(s.title, "CS2");
        assert_eq!(s.level, Some(Yellow), "джиттер от скачка");
        assert_eq!((s.spikes, s.worst), (1, Some(300)));
        assert_eq!(s.blame, Some(Kind::Updates));
        assert_eq!(s.place, Place::Beyond);
        assert_eq!(s.hist.len(), 60);
        assert_eq!(s.hist.iter().flatten().max(), Some(&300));
        assert_eq!(s.gw.map(|g| g.median), Some(2));

        let mut calm = Live::new("cs2.exe", Instant::now());
        for _ in 0..60 {
            calm.push(Some(20), Some(Some(2)));
        }
        calm.seen = vec![Kind::Updates];
        let s = summarize(&calm, 600, "22:00".into());
        assert_eq!(s.level, Some(Green));
        assert_eq!(s.blame, None, "сеть в порядке — никого не виним");
        assert_eq!(s.place, Place::Fine);

        let empty = summarize(&Live::new("cs2.exe", Instant::now()), 600, "22:00".into());
        assert_eq!(empty.level, None);
        assert_eq!(empty.inet, None);
        let mut dead = Live::new("cs2.exe", Instant::now());
        for _ in 0..10 {
            dead.push(None, None);
        }
        assert_eq!(summarize(&dead, 600, "22:00".into()).level, None, "ни одного ответа — пинг не измерен");
    }

    #[test]
    fn summary_toast_good() {
        let s = sum(Some(st(22, 3.0, 60, 0)), Place::Fine, 0, None, None, 4320);
        assert_eq!(summary_toast(&s), ("Игра: пинг 22 мс, джиттер 3, потери 0%".to_string(), "1 ч 12 мин · сеть в порядке".to_string()));
        assert_eq!(summary_line(&s), "сеть в порядке");
    }

    #[test]
    fn summary_toast_spike_blame() {
        let s = sum(Some(st(22, 9.0, 60, 0)), Place::Beyond, 3, Some(300), Some(Kind::Updates), 4320);
        assert_eq!(summary_toast(&s), ("Игра: были скачки пинга до 300 мс".to_string(), "мешал Windows Update · щёлкните — подробности".to_string()));
        assert_eq!(summary_line(&s), "были скачки пинга до 300 мс · мешал Windows Update");
    }

    #[test]
    fn summary_toast_branches() {
        let t = |s: &Summary| summary_toast(s);
        let loss = sum(Some(st(20, 1.0, 100, 3)), Place::Unknown, 2, Some(400), None, 4320);
        assert_eq!(t(&loss).0, "Игра: терялись пакеты — 3%");
        let small_loss = sum(Some(st(20, 1.0, 1000, 10)), Place::Unknown, 0, None, None, 4320);
        assert_eq!(t(&small_loss).0, "Игра: терялись пакеты — 1%");
        let ping = sum(Some(st(120, 3.0, 60, 0)), Place::Unknown, 0, None, None, 4320);
        assert_eq!(t(&ping).0, "Игра: высокий пинг — 120 мс");
        let jit = sum(Some(st(40, 20.0, 60, 0)), Place::Unknown, 0, None, None, 4320);
        assert_eq!(t(&jit).0, "Игра: пинг скакал — джиттер 20 мс");
        assert_eq!(t(&jit).1, "1 ч 12 мин · щёлкните — подробности");
        let home = sum(Some(st(120, 3.0, 60, 0)), Place::Home, 0, None, None, 4320);
        assert_eq!(t(&home).1, "дома: Wi‑Fi или кабель до роутера");
        assert_eq!(summary_line(&home), "высокий пинг — 120 мс · дома: Wi‑Fi или кабель до роутера");
        let beyond = sum(Some(st(120, 3.0, 60, 0)), Place::Beyond, 0, None, None, 4320);
        assert_eq!(t(&beyond).1, "в интернете: провайдер или канал забит");
        let none = sum(None, Place::Unknown, 0, None, None, 4320);
        assert_eq!(t(&none), ("Игра окончена · 1 ч 12 мин".to_string(), "пинг не измерен — интернет не отвечал".to_string()));
        assert_eq!(summary_line(&none), "пинг не измерен");
        let dead = sum(Some(Stats { sent: 10, lost: 10, ..Default::default() }), Place::Unknown, 0, None, None, 600);
        assert_eq!(t(&dead).1, "пинг не измерен — интернет не отвечал");
    }

    #[test]
    fn summary_toast_fits_limits() {
        // самые длинные зелёные: двузначный пинг и потери «<0,1%» или «0,5%»
        let inets = [st(1500, 999.0, 100, 100), st(1500, 999.0, 100, 0), st(60, 5.0, 100, 0), st(60, 4.9, 100_000, 1), st(60, 5.0, 1000, 5), st(9999, 0.0, 14400, 1), st(30, 99.5, 100, 50), Stats { sent: 10, lost: 10, ..Default::default() }];
        let blames = [None, Some(Kind::Updates), Some(Kind::Torrent), Some(Kind::Steam), Some(Kind::Launcher), Some(Kind::OneDrive)];
        for inet in inets {
            for worst in [None, Some(1500), Some(99_999)] {
                for spikes in [0, 1, 999] {
                    for secs in [180, 9 * 3600 + 59 * 60, u64::MAX] {
                        for b in blames {
                            for pl in [Place::Fine, Place::Home, Place::Beyond, Place::Unknown] {
                                let s = sum(Some(inet), pl, spikes, worst, b, secs);
                                let (a, t) = summary_toast(&s);
                                assert!(a.chars().count() <= 44, "{a}");
                                assert!(t.chars().count() <= 56, "{t}");
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn page_height_matches_layout() {
        assert_eq!(page_height(0, false), 426.0);
        assert_eq!(page_height(1, true), 450.0);
        assert_eq!(page_height(4, true), 588.0);
        assert_eq!(page_height(9, true), page_height(4, true));
        // та же сумма, что набирает ui_game: allocate + item_spacing 8 после каждого + два add_space(2)
        let list = |n: usize| if n == 0 { EMPTY_ROW_H } else { n as f32 * ROW_H + (n - 1) as f32 * GAP };
        let manual = |n: usize, last: f32| 104.0 + HERO_H + GAP + GRAPH_H + GAP + 2.0 + LIST_HEAD_H + GAP + list(n) + GAP + 2.0 + last;
        assert_eq!(page_height(2, false), manual(2, LAST_EMPTY_H));
        assert_eq!(page_height(3, true), manual(3, LAST_H));
    }

    #[test]
    fn demo_is_complete() {
        let mut s = State::default();
        fill_demo(&mut s);
        assert_eq!(s.inet_ring.len(), RING);
        assert_eq!(s.gw_ring.len(), RING);
        assert_eq!(s.gw, Some(Ipv4Addr::new(192, 168, 0, 1)));
        assert!(s.culprits.len() >= 2);
        assert!(s.scanned);
        assert_eq!(s.inet_ring[41], Some(140));
        assert_eq!(s.inet_ring[50], None);
        let last = s.last.clone().unwrap();
        assert_eq!(last, demo_summary());
        assert_eq!((last.title.as_str(), last.secs, last.ended_at.as_str()), ("CS2", 4320, "21:40"));
        assert_eq!(last.level, Some(Yellow));
        assert_eq!((last.spikes, last.worst, last.blame), (3, Some(310), Some(Kind::Updates)));
        assert_eq!(last.hist.len(), 60);
        assert_eq!(summary_toast(&last).0, "Игра: были скачки пинга до 310 мс");
        // фон без помех: образцы детерминированы
        let mut again = State::default();
        fill_demo(&mut again);
        assert_eq!(again.inet_ring, s.inet_ring);
    }
}
