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
    let here = chrono::Local::now().offset().local_minus_utc() as f64 / 3600.0 * 15.0;
    let found: Vec<(f64, f64, String)> = v["results"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|r| Some((r["latitude"].as_f64()?, r["longitude"].as_f64()?, r["name"].as_str().unwrap_or(city).to_string())))
                .collect()
        })
        .unwrap_or_default();
    let near = |lon: f64| ((lon - here + 540.0).rem_euclid(360.0) - 180.0).abs() <= 25.0;
    Ok(found.iter().find(|c| near(c.1)).or(found.first()).cloned())
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

pub fn deg(t: f64) -> String {
    let r = t.round() as i64;
    format!("{r}°")
}
