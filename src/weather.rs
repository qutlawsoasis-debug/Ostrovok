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

#[cfg(test)]
mod tests {
    use super::*;
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
}

pub fn deg(t: f64) -> String {
    let r = t.round() as i64;
    format!("{r}°")
}
