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

#[derive(Clone)]
pub struct Now {
    pub city: String,
    pub temp: f64,
    pub feels: f64,
    pub humidity: f64,
    pub wind: f64,
    pub code: i64,
    pub days: Vec<Day>,
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
        "https://geocoding-api.open-meteo.com/v1/search?count=1&language=ru&name={}",
        encode(city)
    ))?;
    let Some(r) = v["results"].get(0) else { return Ok(None) };
    match (r["latitude"].as_f64(), r["longitude"].as_f64()) {
        (Some(lat), Some(lon)) => Ok(Some((lat, lon, r["name"].as_str().unwrap_or(city).to_string()))),
        _ => Ok(None),
    }
}

pub fn fetch(lat: f64, lon: f64, name: &str) -> Result<Now, String> {
    let v = get(&format!(
        "https://api.open-meteo.com/v1/forecast?latitude={lat:.4}&longitude={lon:.4}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,wind_speed_10m,weather_code\
         &daily=temperature_2m_max,temperature_2m_min,weather_code&timezone=auto&forecast_days=5&wind_speed_unit=ms"
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
    Ok(Now {
        city: name.to_string(),
        temp: cur["temperature_2m"].as_f64().ok_or("нет данных")?,
        feels: cur["apparent_temperature"].as_f64().unwrap_or(0.0),
        humidity: cur["relative_humidity_2m"].as_f64().unwrap_or(0.0),
        wind: cur["wind_speed_10m"].as_f64().unwrap_or(0.0),
        code: cur["weather_code"].as_i64().unwrap_or(3),
        days,
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
