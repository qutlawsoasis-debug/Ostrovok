//! Что сейчас играет в системе (Spotify, браузер, плееры) — через системный медиасеанс Windows.
//! Опрос идёт в отдельном потоке, интерфейс только читает готовое состояние.

use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::DataReader;

#[derive(Clone)]
pub struct Info {
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub pos: f64,
    pub dur: f64,
    pub at: Instant,
}

#[derive(Default)]
pub struct State {
    pub info: Option<Info>,
    /// ширина, высота, RGBA
    pub thumb: Option<(usize, usize, Vec<u8>)>,
    pub thumb_gen: u64,
    /// яркий цвет обложки — для эквалайзера, прогресса и свечения
    pub accent: Option<[u8; 3]>,
}

pub enum Cmd {
    Toggle,
    Next,
    Prev,
    /// перемотать на позицию, секунды от начала
    Seek(f64),
}

/// Куда перемотать по щелчку в точке x на полосе [left, left+width] трека длиной dur секунд.
pub fn seek_secs(x: f32, left: f32, width: f32, dur: f64) -> Option<f64> {
    if width <= 0.0 || !(dur > 0.0) {
        return None;
    }
    Some(((x - left) / width).clamp(0.0, 1.0) as f64 * dur)
}

/// Секунды → единицы Windows Media (100 нс); отрицательное — в ноль.
pub fn secs_to_ticks(secs: f64) -> i64 {
    (secs.max(0.0) * 10_000_000.0).round() as i64
}

/// Сколько раз на трек пытаться получить обложку.
const MAX_THUMB_TRIES: u32 = 10;

/// Следующая пауза (с) перед повторным запросом менеджера медиасеансов: 1, 2, 4 … до 30.
fn mgr_backoff(prev: u32) -> u32 {
    if prev == 0 { 1 } else { (prev * 2).min(30) }
}

/// Позиция для показа: сразу после смены трека Windows ещё отдаёт позицию прошлого
/// (та же длительность, далеко от начала) — тогда показываем начало.
/// Не дольше 4 с после смены: иначе трек той же длины, начатый с середины, навсегда показывал бы 0.
pub fn sane_timeline(stale_dur: Option<f64>, pos: f64, dur: f64, since_switch: f64) -> f64 {
    match stale_dur {
        Some(d) if since_switch < 4.0 && (d - dur).abs() < 0.5 && pos > 5.0 => 0.0,
        _ => pos,
    }
}

/// Обрезает строку до max символов (не байт) с «…» на конце.
pub fn ellipsize_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out = out.trim_end().to_string();
    out.push('…');
    out
}

pub fn spawn() -> (Arc<Mutex<State>>, mpsc::Sender<Cmd>) {
    let state = Arc::new(Mutex::new(State::default()));
    let (tx, rx) = mpsc::channel::<Cmd>();
    let shared = state.clone();
    std::thread::spawn(move || {
        let mut mgr: Option<Manager> = None;
        let mut key = String::new();
        let mut have_thumb = false;
        let mut thumb_tries = 0u32;
        // нет менеджера медиасеансов — повторяем запрос всё реже (до 30 с)
        let mut mgr_wait = 0u32;
        let mut mgr_next = Instant::now();
        // длительность прошлого трека: пока Windows отдаёт его позицию для нового — показываем 0
        let mut stale_dur: Option<f64> = None;
        let mut last_dur = 0.0f64;
        let mut switched_at = Instant::now();
        loop {
            let cmd = match rx.recv_timeout(crate::quiet::poll_every(crate::quiet::Src::Media, crate::quiet::in_game())) {
                Ok(c) => Some(c),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if mgr.is_none() && Instant::now() >= mgr_next {
                mgr = Manager::RequestAsync().and_then(|op| op.get()).ok();
                if mgr.is_none() {
                    mgr_wait = mgr_backoff(mgr_wait);
                    mgr_next = Instant::now() + Duration::from_secs(mgr_wait as u64);
                } else {
                    mgr_wait = 0;
                }
            }
            let Some(m) = &mgr else { continue };
            let session = match m.GetCurrentSession() {
                Ok(s) => Some(s),
                Err(e) => {
                    // «нет сеанса» приходит пустой ошибкой; настоящая (перезапустился explorer, RPC отвалился)
                    // означает, что менеджер мёртв — без сброса музыка пропадала до перезапуска острова
                    if e.code().is_err() {
                        mgr = None;
                        mgr_next = Instant::now() + Duration::from_secs(2);
                    }
                    None
                }
            };

            if let (Some(c), Some(s)) = (&cmd, &session) {
                let _ = match c {
                    Cmd::Toggle => s.TryTogglePlayPauseAsync().and_then(|o| o.get()),
                    Cmd::Next => s.TrySkipNextAsync().and_then(|o| o.get()),
                    Cmd::Prev => s.TrySkipPreviousAsync().and_then(|o| o.get()),
                    // плеер может не уметь перематывать — тогда Windows просто ответит отказом
                    Cmd::Seek(secs) => s.TryChangePlaybackPositionAsync(secs_to_ticks(*secs)).and_then(|o| o.get()),
                };
                std::thread::sleep(Duration::from_millis(120));
            }

            let polled = session.as_ref().and_then(|s| poll(s).ok());
            match polled {
                None => {
                    let mut st = shared.lock().unwrap();
                    if st.info.is_some() || st.thumb.is_some() {
                        st.info = None;
                        st.thumb = None;
                        st.accent = None;
                        st.thumb_gen += 1;
                    }
                    key.clear();
                    have_thumb = false;
                }
                Some(mut info) => {
                    let new_key = format!("{}|{}", info.title, info.artist);
                    if new_key != key {
                        stale_dur = (!key.is_empty()).then_some(last_dur);
                        switched_at = Instant::now();
                        key = new_key;
                        have_thumb = false;
                        thumb_tries = 0;
                        let mut st = shared.lock().unwrap();
                        st.thumb = None;
                        st.accent = None;
                        st.thumb_gen += 1;
                    }
                    last_dur = info.dur;
                    let pos = sane_timeline(stale_dur, info.pos, info.dur, switched_at.elapsed().as_secs_f64());
                    if pos == info.pos {
                        stale_dur = None;
                    }
                    info.pos = pos;
                    shared.lock().unwrap().info = Some(info);
                    if !have_thumb && thumb_tries < MAX_THUMB_TRIES {
                        thumb_tries += 1;
                        // обложка могла прийти от прошлого трека — берём только с меткой текущего
                        if let Some((t, _, _)) = session.as_ref().and_then(|s| thumbnail(s, &key).ok()) {
                            have_thumb = true;
                            let mut st = shared.lock().unwrap();
                            st.accent = dominant(&t.2);
                            st.thumb = Some(t);
                            st.thumb_gen += 1;
                        }
                    }
                }
            }
        }
    });
    (state, tx)
}

fn poll(s: &Session) -> windows::core::Result<Info> {
    let p = s.TryGetMediaPropertiesAsync()?.get()?;
    let title = clean_title(&p.Title()?.to_string());
    if title.trim().is_empty() {
        return Err(windows::core::Error::empty());
    }
    let artist = clean_artist(&p.Artist().map(|a| a.to_string()).unwrap_or_default());
    let playing = s
        .GetPlaybackInfo()
        .and_then(|i| i.PlaybackStatus())
        .map(|st| st == Status::Playing)
        .unwrap_or(false);

    let (mut pos, mut dur) = (0.0, 0.0);
    if let Ok(tl) = s.GetTimelineProperties() {
        let start = tl.StartTime().map(|t| t.Duration).unwrap_or(0);
        let end = tl.EndTime().map(|t| t.Duration).unwrap_or(0);
        let mut p100 = tl.Position().map(|t| t.Duration).unwrap_or(0);
        if playing {
            if let Ok(upd) = tl.LastUpdatedTime() {
                // отсчёт Windows — от 1601 года, шаг 100 нс
                let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() / 100).unwrap_or(0) as i64
                    + 116_444_736_000_000_000;
                let delta = now - upd.UniversalTime;
                if upd.UniversalTime > 116_444_736_000_000_000 && delta > 0 {
                    p100 += delta;
                }
            }
        }
        if end > start {
            dur = (end - start) as f64 / 1e7;
            pos = (p100 as f64 / 1e7).clamp(0.0, dur);
        }
    }
    Ok(Info { title, artist, playing, pos, dur, at: Instant::now() })
}

type Thumb = (usize, usize, Vec<u8>);

/// Ключ трека в том же виде, что строит опрос.
fn track_key(title: &str, artist: &str) -> String {
    format!("{}|{}", clean_title(title), clean_artist(artist))
}

/// Обложка с «сырыми» заголовком и исполнителем относится к текущему треку.
fn thumb_matches(key: &str, title: &str, artist: &str) -> bool {
    !key.is_empty() && track_key(title, artist) == key
}

/// Доля прошедшего трека в 0..1; 0 при неизвестной длительности.
pub fn track_frac(pos: f64, elapsed: f64, dur: f64, playing: bool) -> f32 {
    if !(dur > 0.0) {
        return 0.0;
    }
    let at = if playing { pos + elapsed } else { pos };
    ((at.min(dur) / dur) as f32).clamp(0.0, 1.0)
}

/// Обложка вместе с заголовком и исполнителем из тех же свойств.
fn thumbnail(s: &Session, key: &str) -> Result<(Thumb, String, String), String> {
    let e = |e: windows::core::Error| e.to_string();
    let p = s.TryGetMediaPropertiesAsync().map_err(e)?.get().map_err(e)?;
    let title = p.Title().map(|t| t.to_string()).unwrap_or_default();
    let artist = p.Artist().map(|a| a.to_string()).unwrap_or_default();
    // свойства ещё от прошлого трека — не читаем и не декодируем картинку зря
    if !thumb_matches(key, &title, &artist) {
        return Err("обложка другого трека".into());
    }
    let stream = p.Thumbnail().map_err(e)?.OpenReadAsync().map_err(e)?.get().map_err(e)?;
    let size = stream.Size().map_err(e)? as u32;
    if size == 0 || size > 8_000_000 {
        return Err("пустая обложка".into());
    }
    let reader = DataReader::CreateDataReader(&stream).map_err(e)?;
    reader.LoadAsync(size).map_err(e)?.get().map_err(e)?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf).map_err(e)?;
    let img = image::load_from_memory(&buf).map_err(|e| e.to_string())?;
    let img = img.thumbnail(160, 160).to_rgba8();
    Ok(((img.width() as usize, img.height() as usize, img.into_raw()), title, artist))
}

/// Текст песни с отметками времени с lrclib.net (бесплатно, без ключа).
/// Наружу уходят только исполнитель и название трека.
pub fn fetch_lyrics(artist: &str, title: &str) -> Option<Vec<(f64, String)>> {
    let url = format!(
        "https://lrclib.net/api/get?artist_name={}&track_name={}",
        crate::weather::encode(artist),
        crate::weather::encode(title)
    );
    let v: serde_json::Value = ureq::get(&url).timeout(Duration::from_secs(8)).set("user-agent", "desktop-widget").call().ok()?.into_json().ok()?;
    let synced = v["syncedLyrics"].as_str()?;
    let mut out = Vec::new();
    for line in synced.lines() {
        // формат строки: [мм:сс.сс] текст
        let Some(rest) = line.strip_prefix('[') else { continue };
        let Some((stamp, text)) = rest.split_once(']') else { continue };
        let Some((mm, ss)) = stamp.split_once(':') else { continue };
        if let (Ok(mm), Ok(ss)) = (mm.parse::<f64>(), ss.parse::<f64>()) {
            let text = text.trim();
            if !text.is_empty() {
                out.push((mm * 60.0 + ss, text.to_string()));
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Чистит название из вкладки браузера: счётчик уведомлений «(18) », хвост « - YouTube» и т. п.
pub fn clean_title(raw: &str) -> String {
    let mut t = raw.trim();
    // «(18) …», «(3)…», «• …» — счётчики и точки непрочитанного в начале
    loop {
        let before = t;
        if let Some(rest) = t.strip_prefix('(') {
            if let Some((num, after)) = rest.split_once(')') {
                if !num.is_empty() && num.chars().all(|c| c.is_ascii_digit() || c == '+') {
                    t = after.trim_start();
                }
            }
        }
        t = t.trim_start_matches(['•', '●', '▶', '►']).trim_start();
        if t == before {
            break;
        }
    }
    const TAILS: [&str; 12] = [
        "YouTube Music", "YouTube", "TikTok", "SoundCloud", "Яндекс Музыка", "Яндекс.Музыка",
        "VK Музыка", "ВКонтакте", "Twitch", "Spotify", "Deezer", "Rutube",
    ];
    for _ in 0..2 {
        for tail in TAILS {
            for sep in [" - ", " — ", " – ", " | ", " · "] {
                let suffix = format!("{sep}{tail}");
                if t.len() > suffix.len() && t.ends_with(&suffix) {
                    t = t[..t.len() - suffix.len()].trim_end();
                }
            }
        }
    }
    t.to_string()
}

/// Исполнитель: YouTube отдаёт «Канал - Topic», это не часть имени.
pub fn clean_artist(raw: &str) -> String {
    raw.trim().trim_end_matches(" - Topic").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_only_for_current_track() {
        assert!(thumb_matches("Song|Band", "Song", "Band"));
        assert!(!thumb_matches("Song|Band", "Other", "Band"));
        assert!(thumb_matches("Song|Band", "(3) Song - YouTube", "Band"));
        assert!(!thumb_matches("", "Song", "Band"));
    }

    #[test]
    fn frac_bounds() {
        assert_eq!(track_frac(10.0, 0.0, 0.0, true), 0.0);
        assert_eq!(track_frac(10.0, 0.0, f64::NAN, true), 0.0);
        assert_eq!(track_frac(50.0, 0.0, 100.0, false), 0.5);
        assert_eq!(track_frac(50.0, 25.0, 100.0, true), 0.75);
        assert_eq!(track_frac(50.0, 25.0, 100.0, false), 0.5);
        assert_eq!(track_frac(90.0, 30.0, 100.0, true), 1.0);
        assert_eq!(track_frac(-5.0, 0.0, 100.0, false), 0.0);
    }

    #[test]
    fn seek_by_click() {
        // полоса от x=100 шириной 200, трек 240 с
        assert_eq!(seek_secs(100.0, 100.0, 200.0, 240.0), Some(0.0));
        assert_eq!(seek_secs(200.0, 100.0, 200.0, 240.0), Some(120.0));
        assert_eq!(seek_secs(300.0, 100.0, 200.0, 240.0), Some(240.0));
        assert_eq!(seek_secs(50.0, 100.0, 200.0, 240.0), Some(0.0), "левее полосы — начало");
        assert_eq!(seek_secs(999.0, 100.0, 200.0, 240.0), Some(240.0), "правее — конец");
        assert_eq!(seek_secs(150.0, 100.0, 200.0, 0.0), None, "длительность неизвестна");
        assert_eq!(seek_secs(150.0, 100.0, 0.0, 240.0), None);
        assert_eq!(seek_secs(150.0, 100.0, 200.0, f64::NAN), None);
    }

    #[test]
    fn ticks() {
        assert_eq!(secs_to_ticks(1.5), 15_000_000);
        assert_eq!(secs_to_ticks(0.0), 0);
        assert_eq!(secs_to_ticks(-3.0), 0);
    }

    #[test]
    fn titles() {
        assert_eq!(clean_title("(18)TikTok - Make Your Day"), "TikTok - Make Your Day");
        assert_eq!(clean_title("(3) Linkin Park - Numb - YouTube"), "Linkin Park - Numb");
        assert_eq!(clean_title("• Song — Яндекс Музыка"), "Song");
        assert_eq!(clean_title("Numb (Live)"), "Numb (Live)");
        assert_eq!(clean_title("(Intro)"), "(Intro)");
        assert_eq!(clean_title("YouTube"), "YouTube");
        assert_eq!(clean_title("(99+) Trap mix | SoundCloud"), "Trap mix");
        assert_eq!(clean_artist("Daft Punk - Topic"), "Daft Punk");
        assert_eq!(clean_title("Кино — Группа крови — Яндекс Музыка"), "Кино — Группа крови");
    }

    #[test]
    fn ellipsize() {
        assert_eq!(ellipsize_chars("Коротко", 20), "Коротко");
        assert_eq!(ellipsize_chars("Группа крови", 7), "Группа…");
        assert_eq!(ellipsize_chars("🎵🎶🎵🎶", 3), "🎵🎶…");
        assert_eq!(ellipsize_chars("abc", 3), "abc");
        assert_eq!(ellipsize_chars("abcd", 0), "");
    }

    #[test]
    fn timeline_after_switch() {
        assert_eq!(sane_timeline(Some(200.0), 120.0, 200.0, 1.0), 0.0, "старая позиция при той же длительности");
        assert_eq!(sane_timeline(Some(200.0), 3.0, 200.0, 1.0), 3.0, "уже с начала");
        assert_eq!(sane_timeline(Some(200.0), 120.0, 180.0, 1.0), 120.0, "длительность сменилась — позиция свежая");
        assert_eq!(sane_timeline(None, 120.0, 200.0, 1.0), 120.0);
    }

    #[test]
    fn backoff() {
        assert_eq!(mgr_backoff(0), 1);
        assert_eq!(mgr_backoff(1), 2);
        assert_eq!(mgr_backoff(2), 4);
        assert_eq!(mgr_backoff(16), 30);
        assert_eq!(mgr_backoff(30), 30);
    }
}

/// Яркий цвет обложки: насыщенные и светлые точки весят больше (приём из Atoll, MainWindow.Liquid.cs, MIT).
fn dominant(rgba: &[u8]) -> Option<[u8; 3]> {
    let (mut r, mut g, mut b, mut total) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for px in rgba.chunks_exact(4).step_by(7) {
        let (rr, gg, bb) = (px[0] as f64, px[1] as f64, px[2] as f64);
        let max = rr.max(gg).max(bb);
        let min = rr.min(gg).min(bb);
        let weight = 0.05 + (max - min) / 255.0 * (max / 255.0);
        r += rr * weight;
        g += gg * weight;
        b += bb * weight;
        total += weight;
    }
    if total <= 0.0 {
        return None;
    }
    let (r, g, b) = (r / total, g / total, b / total);
    let top = r.max(g).max(b);
    if top < 1.0 {
        return None;
    }
    // достаточно светлый, чтобы читаться на тёмном острове
    let boost = 235.0 / top;
    Some([(r * boost).min(255.0) as u8, (g * boost).min(255.0) as u8, (b * boost).min(255.0) as u8])
}
