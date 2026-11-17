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
}

pub fn spawn() -> (Arc<Mutex<State>>, mpsc::Sender<Cmd>) {
    let state = Arc::new(Mutex::new(State::default()));
    let (tx, rx) = mpsc::channel::<Cmd>();
    let shared = state.clone();
    std::thread::spawn(move || {
        let mut mgr: Option<Manager> = None;
        let mut key = String::new();
        let mut have_thumb = false;
        loop {
            let cmd = match rx.recv_timeout(Duration::from_millis(1000)) {
                Ok(c) => Some(c),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if mgr.is_none() {
                mgr = Manager::RequestAsync().and_then(|op| op.get()).ok();
            }
            let Some(m) = &mgr else { continue };
            let session = m.GetCurrentSession().ok();

            if let (Some(c), Some(s)) = (&cmd, &session) {
                let _ = match c {
                    Cmd::Toggle => s.TryTogglePlayPauseAsync().and_then(|o| o.get()),
                    Cmd::Next => s.TrySkipNextAsync().and_then(|o| o.get()),
                    Cmd::Prev => s.TrySkipPreviousAsync().and_then(|o| o.get()),
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
                Some(info) => {
                    let new_key = format!("{}|{}", info.title, info.artist);
                    if new_key != key {
                        key = new_key;
                        have_thumb = false;
                        let mut st = shared.lock().unwrap();
                        st.thumb = None;
                        st.accent = None;
                        st.thumb_gen += 1;
                    }
                    shared.lock().unwrap().info = Some(info);
                    if !have_thumb {
                        if let Some(t) = session.as_ref().and_then(|s| thumbnail(s).ok()) {
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

fn thumbnail(s: &Session) -> Result<(usize, usize, Vec<u8>), String> {
    let e = |e: windows::core::Error| e.to_string();
    let p = s.TryGetMediaPropertiesAsync().map_err(e)?.get().map_err(e)?;
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
    Ok((img.width() as usize, img.height() as usize, img.into_raw()))
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
    fn titles() {
        assert_eq!(clean_title("(18)TikTok - Make Your Day"), "TikTok - Make Your Day");
        assert_eq!(clean_title("(3) Linkin Park - Numb - YouTube"), "Linkin Park - Numb");
        assert_eq!(clean_title("• Song — Яндекс Музыка"), "Song");
        assert_eq!(clean_title("Numb (Live)"), "Numb (Live)");
        assert_eq!(clean_title("(Intro)"), "(Intro)");
        assert_eq!(clean_title("YouTube"), "YouTube");
        assert_eq!(clean_title("(99+) Trap mix | SoundCloud"), "Trap mix");
        assert_eq!(clean_artist("Daft Punk - Topic"), "Daft Punk");
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
