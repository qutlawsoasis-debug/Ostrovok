//! Погода с open-meteo.com: без ключа и регистрации.
//! Наружу уходит только название города и его координаты.

use serde_json::Value;

#[derive(Clone)]
pub struct Day {
    pub label: String,
    pub min: f64,
    pub max: f64,
    pub code: i64,
}

/// Прогноз на один час.
#[derive(Clone)]
pub struct Hour {
    /// час суток, 0…23
    pub hour: u32,
    pub temp: f64,
    /// вероятность осадков, %
    pub rain: f64,
    pub code: i64,
}

#[derive(Clone)]
pub struct Now {
    pub city: String,
    pub temp: f64,
    pub feels: f64,
    pub humidity: f64,
    pub wind: f64,
    pub code: i64,
    pub days: Vec<Day>,
    /// ближайшие сутки по часам, начиная с текущего
    pub hours: Vec<Hour>,
    /// восход и закат сегодня, «07:41»
    pub sunrise: String,
    pub sunset: String,
    /// смещение часового пояса города от UTC в секундах (восход и закат даны в его времени)
    pub utc_offset: i32,
}

pub fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn get(url: &str) -> Result<Value, String> {
    ureq::get(url)
        .timeout(std::time::Duration::from_secs(10))
        .call()
        .map_err(|e| e.to_string())?
        .into_json::<Value>()
        .map_err(|e| e.to_string())
}

pub fn geocode(city: &str) -> Result<Option<(f64, f64, String)>, String> {
    let v = get(&format!(
        "https://geocoding-api.open-meteo.com/v1/search?count=6&language=ru&name={}",
        encode(city)
    ))?;
    // Городов с одним названием бывает несколько (Минден есть и в Германии, и в США).
    // Берём тот, что лежит в поясе часов этого компьютера: его долгота близка к смещению от Гринвича × 15°.
    let found: Vec<(f64, f64, String)> = v["results"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|r| Some((r["latitude"].as_f64()?, r["longitude"].as_f64()?, r["name"].as_str().unwrap_or(city).to_string())))
                .collect()
        })
        .unwrap_or_default();
    let offset = local_offset_hours();
    Ok(found.iter().find(|c| fits_zone(c.1, offset)).or(found.first()).cloned())
}

pub fn local_offset_hours() -> f64 {
    chrono::Local::now().offset().local_minus_utc() as f64 / 3600.0
}

/// Похожа ли долгота на часовой пояс: долгота примерно равна смещению от Гринвича × 15°.
/// Запас широкий (пояса кривые, летнее время), но Германию от США отличает уверенно.
pub fn fits_zone(lon: f64, offset_hours: f64) -> bool {
    ((lon - offset_hours * 15.0 + 540.0).rem_euclid(360.0) - 180.0).abs() <= 25.0
}

pub fn fetch(lat: f64, lon: f64, name: &str) -> Result<Now, String> {
    let v = get(&format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat:.4}&longitude={lon:.4}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,wind_speed_10m,weather_code\
         &daily=temperature_2m_max,temperature_2m_min,weather_code,sunrise,sunset\
         &hourly=temperature_2m,precipitation_probability,weather_code&forecast_hours=24\
         &timezone=auto&forecast_days=5&wind_speed_unit=ms"
    ))?;
    let cur = &v["current"];
    let daily = &v["daily"];
    let mut days = Vec::new();
    if let Some(times) = daily["time"].as_array() {
        for (i, t) in times.iter().enumerate() {
            days.push(Day {
                label: weekday(t.as_str().unwrap_or("")),
                min: daily["temperature_2m_min"][i].as_f64().unwrap_or(0.0),
                max: daily["temperature_2m_max"][i].as_f64().unwrap_or(0.0),
                code: daily["weather_code"][i].as_i64().unwrap_or(3),
            });
        }
    }
    let hourly = &v["hourly"];
    let mut hours = Vec::new();
    if let Some(times) = hourly["time"].as_array() {
        for (i, t) in times.iter().enumerate().take(24) {
            // время приходит как «2026-11-16T15:00»
            let hour = t.as_str().and_then(|s| s.get(11..13)).and_then(|h| h.parse().ok()).unwrap_or(0);
            let Some(temp) = hourly["temperature_2m"][i].as_f64() else { continue };
            hours.push(Hour {
                hour,
                temp,
                rain: hourly["precipitation_probability"][i].as_f64().unwrap_or(0.0),
                code: hourly["weather_code"][i].as_i64().unwrap_or(3),
            });
        }
    }
    let clock = |v: &Value| v.as_str().and_then(|s| s.get(11..16)).unwrap_or("").to_string();
    Ok(Now {
        city: name.to_string(),
        temp: cur["temperature_2m"].as_f64().ok_or("нет данных")?,
        feels: cur["apparent_temperature"].as_f64().unwrap_or(0.0),
        humidity: cur["relative_humidity_2m"].as_f64().unwrap_or(0.0),
        wind: cur["wind_speed_10m"].as_f64().unwrap_or(0.0),
        code: cur["weather_code"].as_i64().unwrap_or(3),
        days,
        hours,
        sunrise: clock(&daily["sunrise"][0]),
        sunset: clock(&daily["sunset"][0]),
        utc_offset: v["utc_offset_seconds"].as_i64().unwrap_or(0) as i32,
    })
}

fn weekday(date: &str) -> String {
    use chrono::Datelike;
    match chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
        Ok(d) => ["Пн", "Вт", "Ср", "Чт", "Пт", "Сб", "Вс"][d.weekday().num_days_from_monday() as usize].to_string(),
        Err(_) => "—".to_string(),
    }
}

pub fn describe(code: i64) -> (&'static str, &'static str) {
    match code {
        0 => ("☀", "Ясно"),
        1 | 2 => ("⛅", "Малооблачно"),
        3 => ("☁", "Пасмурно"),
        45 | 48 => ("≋", "Туман"),
        51..=57 => ("☂", "Морось"),
        61..=67 => ("☂", "Дождь"),
        71..=77 => ("❄", "Снег"),
        80..=82 => ("☂", "Ливень"),
        85 | 86 => ("❄", "Снегопад"),
        95..=99 => ("⚡", "Гроза"),
        _ => ("☁", "Облачно"),
    }
}

/// Цвет значка погоды: солнце золотое, облака серо-голубые, дождь синий, гроза сиреневая.
pub fn tint(code: i64) -> [u8; 3] {
    match code {
        0 => [255, 204, 92],
        1 | 2 => [255, 226, 160],
        3 | 45 | 48 => [196, 206, 226],
        51..=67 | 80..=82 => [120, 182, 255],
        71..=77 | 85 | 86 => [220, 236, 255],
        95..=99 => [196, 150, 255],
        _ => [196, 206, 226],
    }
}

/// Цвет температуры для полоски диапазона: от холодного синего через бирюзу и жёлтый к оранжевому.
pub fn temp_rgb(t: f64) -> [u8; 3] {
    const STOPS: [(f64, [f64; 3]); 5] = [
        (-15.0, [110.0, 140.0, 255.0]),
        (0.0, [110.0, 200.0, 255.0]),
        (10.0, [120.0, 225.0, 190.0]),
        (20.0, [255.0, 214.0, 100.0]),
        (30.0, [255.0, 140.0, 80.0]),
    ];
    let t = t.clamp(STOPS[0].0, STOPS[4].0);
    let i = STOPS.windows(2).position(|w| t <= w[1].0).unwrap_or(3);
    let (a, b) = (STOPS[i], STOPS[i + 1]);
    let k = (t - a.0) / (b.0 - a.0);
    let c = |j: usize| (a.1[j] + (b.1[j] - a.1[j]) * k).round() as u8;
    [c(0), c(1), c(2)]
}

/// Вид подсказки — по нему интерфейс выбирает значок и цвет.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tip {
    Rain,
    Snow,
    Cold,
    Hot,
    Wind,
}

/// Короткие подсказки «что надеть и взять» на ближайшие 12 часов: осадки, холод или жара, ветер. Не больше двух.
pub fn advice(feels: f64, wind: f64, hours: &[Hour]) -> Vec<String> {
    advice_tips(feels, wind, hours).into_iter().map(|t| t.1).collect()
}

/// То же, что advice, но с видом каждой подсказки.
pub fn advice_tips(feels: f64, wind: f64, hours: &[Hour]) -> Vec<(Tip, String)> {
    let soon = &hours[..hours.len().min(12)];
    let mut out = Vec::new();
    if let Some((i, h)) = soon.iter().enumerate().find(|(_, h)| h.rain >= 50.0) {
        let snow = matches!(h.code, 71..=77 | 85 | 86);
        let text = match (snow, i == 0) {
            (true, true) => "Сейчас снег — обувь, которая не промокнет".into(),
            (true, false) => format!("Снег около {:02}:00 — обувь, которая не промокнет", h.hour),
            (false, true) => "Сейчас дождь — возьмите зонт".into(),
            (false, false) => format!("Дождь около {:02}:00 — возьмите зонт", h.hour),
        };
        out.push((if snow { Tip::Snow } else { Tip::Rain }, text));
    }
    let cold = soon.iter().map(|h| h.temp).fold(feels, f64::min);
    if cold <= -10.0 {
        out.push((Tip::Cold, format!("Мороз до {}° — шапка и тёплая куртка", (cold.round() as i64).to_string().replace('-', "−"))));
    } else if cold <= 5.0 {
        out.push((Tip::Cold, "Прохладно — возьмите куртку".into()));
    } else if feels >= 27.0 {
        out.push((Tip::Hot, "Жарко — вода и лёгкая одежда".into()));
    }
    if wind >= 10.0 {
        out.push((Tip::Wind, format!("Сильный ветер {wind:.0} м/с")));
    }
    out.truncate(2);
    out
}

/// Подписи на месте погоды, пока данных нет: (заголовок, пояснение).
pub fn empty_text(city_set: bool, err: Option<&str>) -> (&'static str, &'static str) {
    match (city_set, err) {
        (false, _) => ("Город не указан", "Погода появится, когда вы впишете город в настройках."),
        (true, Some(_)) => ("Нет связи с сервером погоды", "Попробую снова через минуту"),
        (true, None) => ("Загружаю погоду…", ""),
    }
}

/// Через сколько снова спрашивать погоду: 15 минут в норме, после сбоев 1, 2, 4… минуты, не больше 15.
pub fn retry_after(fails: u32) -> std::time::Duration {
    const MAX: u64 = 900;
    if fails == 0 {
        return std::time::Duration::from_secs(MAX);
    }
    let secs = 1u64.checked_shl(fails - 1).and_then(|m| m.checked_mul(60)).map_or(MAX, |v| v.min(MAX));
    std::time::Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_texts() {
        assert_eq!(empty_text(false, None).0, "Город не указан");
        assert_eq!(empty_text(false, Some("x")).0, "Город не указан");
        assert_eq!(empty_text(true, Some("x")), ("Нет связи с сервером погоды", "Попробую снова через минуту"));
        assert_eq!(empty_text(true, None), ("Загружаю погоду…", ""));
    }
    #[test]
    fn retry_backoff() {
        use std::time::Duration as D;
        assert_eq!(retry_after(0), D::from_secs(900));
        assert_eq!(retry_after(1), D::from_secs(60));
        assert_eq!(retry_after(2), D::from_secs(120));
        assert_eq!(retry_after(4), D::from_secs(480));
        assert_eq!(retry_after(5), D::from_secs(900));
        assert_eq!(retry_after(64), D::from_secs(900));
        assert_eq!(retry_after(u32::MAX), D::from_secs(900));
    }
    #[test]
    fn colors() {
        assert_eq!(temp_rgb(-40.0), [110, 140, 255]);
        assert_eq!(temp_rgb(0.0), [110, 200, 255]);
        assert_eq!(temp_rgb(50.0), [255, 140, 80]);
        assert_eq!(temp_rgb(15.0), [188, 220, 145]);
        assert_eq!(tint(0), [255, 204, 92]);
        assert_eq!(tint(63), [120, 182, 255]);
    }
    #[test]
    fn zones() {
        assert!(fits_zone(8.9, 1.0)); // Минден, Германия, зимой
        assert!(fits_zone(8.9, 2.0)); // летом
        assert!(!fits_zone(-93.3, 1.0)); // Минден, Луизиана, с немецкого компьютера
        assert!(fits_zone(37.6, 3.0)); // Москва
        assert!(fits_zone(131.9, 10.0)); // Владивосток
        assert!(fits_zone(179.0, -11.0) || fits_zone(179.0, 12.0)); // через линию смены дат
    }

    fn hours(spec: &[(u32, f64, f64, i64)]) -> Vec<Hour> {
        spec.iter().map(|&(hour, temp, rain, code)| Hour { hour, temp, rain, code }).collect()
    }

    #[test]
    fn advice_rain_later_and_now() {
        let h = hours(&[(14, 12.0, 0.0, 3), (15, 12.0, 20.0, 3), (16, 11.0, 70.0, 61), (17, 10.0, 80.0, 63)]);
        assert_eq!(advice(12.0, 3.0, &h), vec!["Дождь около 16:00 — возьмите зонт".to_string()]);
        let h = hours(&[(9, 12.0, 60.0, 61), (10, 12.0, 60.0, 61)]);
        assert_eq!(advice(12.0, 3.0, &h), vec!["Сейчас дождь — возьмите зонт".to_string()]);
        let h = hours(&[(9, -3.0, 0.0, 3), (10, -3.0, 65.0, 73)]);
        assert_eq!(advice(-3.0, 2.0, &h)[0], "Снег около 10:00 — обувь, которая не промокнет");
    }

    #[test]
    fn advice_temperature_wind_and_limits() {
        // дождь дальше 12 часов не считается
        let mut h = hours(&[(0, 18.0, 0.0, 1); 13]);
        h.push(Hour { hour: 13, temp: 18.0, rain: 90.0, code: 61 });
        assert!(advice(18.0, 2.0, &h).is_empty(), "погода комфортная — молчим");
        assert_eq!(advice(-15.0, 2.0, &hours(&[(8, -12.0, 0.0, 0), (9, -16.0, 0.0, 0)])), vec!["Мороз до −16° — шапка и тёплая куртка".to_string()]);
        assert_eq!(advice(4.0, 2.0, &[]), vec!["Прохладно — возьмите куртку".to_string()]);
        assert_eq!(advice(29.0, 2.0, &[]), vec!["Жарко — вода и лёгкая одежда".to_string()]);
        assert_eq!(advice(15.0, 12.4, &[]), vec!["Сильный ветер 12 м/с".to_string()]);
        // не больше двух подсказок
        let h = hours(&[(9, 2.0, 80.0, 61)]);
        assert_eq!(advice(-12.0, 15.0, &h).len(), 2);
        let kinds: Vec<Tip> = advice_tips(-12.0, 15.0, &h).into_iter().map(|t| t.0).collect();
        assert_eq!(kinds, vec![Tip::Rain, Tip::Cold]);
        assert_eq!(advice_tips(30.0, 11.0, &[]).iter().map(|t| t.0).collect::<Vec<_>>(), vec![Tip::Hot, Tip::Wind]);
    }
}

pub fn deg(t: f64) -> String {
    let r = t.round() as i64;
    format!("{r}°")
}
