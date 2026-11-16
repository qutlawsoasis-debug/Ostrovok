//! Конвертер для строки поиска: «5 км в мили», «72 f в c», «100 долларов в рубли».
//! Единицы считаются на месте. Курсы валют берутся раз в полсуток с open.er-api.com
//! (Rates By Exchange Rate API, https://www.exchangerate-api.com) — туда не уходит ничего, кроме самого запроса.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Length,
    Mass,
    Volume,
    Data,
    Time,
    Speed,
    Temp,
    Money,
}

struct Unit {
    kind: Kind,
    /// сколько базовых единиц в одной этой (для температуры и валют не используется)
    k: f64,
    /// как подписать результат
    label: &'static str,
    /// код валюты или буква шкалы температуры
    code: &'static str,
    /// во что переводить, если цель не названа
    default: &'static str,
}

/// Узнаёт единицу по слову. Слова сравниваются без регистра и без точки на конце;
/// у русских названий достаточно начала слова («километров», «километра» → «километр»).
fn unit(word: &str) -> Option<Unit> {
    let w = word.trim().trim_end_matches('.').to_lowercase();
    let u = |kind, k, label, default| Some(Unit { kind, k, label, code: "", default });
    let starts = |p: &str| w.starts_with(p);
    // сначала точные короткие обозначения
    let exact = match w.as_str() {
        "мм" | "mm" => u(Kind::Length, 0.001, "мм", "см"),
        "см" | "cm" => u(Kind::Length, 0.01, "см", "дюйм"),
        "м" | "m" => u(Kind::Length, 1.0, "м", "фут"),
        "км" | "km" => u(Kind::Length, 1000.0, "км", "миля"),
        "in" | "\"" | "″" => u(Kind::Length, 0.0254, "дюйм.", "см"),
        "ft" | "'" => u(Kind::Length, 0.3048, "фут.", "м"),
        "yd" => u(Kind::Length, 0.9144, "ярд.", "м"),
        "mi" => u(Kind::Length, 1609.344, "миль", "км"),
        "мг" | "mg" => u(Kind::Mass, 1e-6, "мг", "г"),
        "г" | "гр" | "g" => u(Kind::Mass, 0.001, "г", "унция"),
        "кг" | "kg" => u(Kind::Mass, 1.0, "кг", "фунт"),
        "т" | "t" => u(Kind::Mass, 1000.0, "т", "кг"),
        "lb" | "lbs" => u(Kind::Mass, 0.453_592_37, "фунт.", "кг"),
        "oz" => u(Kind::Mass, 0.028_349_523, "унц.", "г"),
        "мл" | "ml" => u(Kind::Volume, 0.001, "мл", "л"),
        "л" | "l" => u(Kind::Volume, 1.0, "л", "галлон"),
        "gal" => u(Kind::Volume, 3.785_411_784, "галл.", "л"),
        "б" | "b" => u(Kind::Data, 1.0, "байт", "кб"),
        "кб" | "kb" => u(Kind::Data, 1024.0, "КБ", "мб"),
        "мб" | "mb" => u(Kind::Data, 1024.0 * 1024.0, "МБ", "гб"),
        "гб" | "gb" => u(Kind::Data, 1024.0 * 1024.0 * 1024.0, "ГБ", "мб"),
        "тб" | "tb" => u(Kind::Data, 1024.0f64.powi(4), "ТБ", "гб"),
        "с" | "сек" | "s" | "sec" => u(Kind::Time, 1.0, "с", "мин"),
        "мин" | "min" => u(Kind::Time, 60.0, "мин", "ч"),
        "ч" | "h" => u(Kind::Time, 3600.0, "ч", "мин"),
        "д" | "дн" | "d" => u(Kind::Time, 86400.0, "дн.", "ч"),
        "нед" => u(Kind::Time, 604_800.0, "нед.", "дн"),
        "км/ч" | "kmh" | "km/h" | "кмч" => u(Kind::Speed, 1.0 / 3.6, "км/ч", "м/с"),
        "м/с" | "m/s" => u(Kind::Speed, 1.0, "м/с", "км/ч"),
        "mph" => u(Kind::Speed, 0.447_04, "миль/ч", "км/ч"),
        "c" | "°c" | "с°" | "°с" | "ц" => Some(Unit { kind: Kind::Temp, k: 1.0, label: "°C", code: "c", default: "f" }),
        "f" | "°f" | "ф" => Some(Unit { kind: Kind::Temp, k: 1.0, label: "°F", code: "f", default: "c" }),
        "k" | "к" => Some(Unit { kind: Kind::Temp, k: 1.0, label: "K", code: "k", default: "c" }),
        _ => None,
    };
    if exact.is_some() {
        return exact;
    }
    // длинные названия — по началу слова
    let named = if starts("миллиметр") {
        u(Kind::Length, 0.001, "мм", "см")
    } else if starts("сантиметр") {
        u(Kind::Length, 0.01, "см", "дюйм")
    } else if starts("километр") {
        u(Kind::Length, 1000.0, "км", "миля")
    } else if starts("метр") {
        u(Kind::Length, 1.0, "м", "фут")
    } else if starts("дюйм") || starts("inch") {
        u(Kind::Length, 0.0254, "дюйм.", "см")
    } else if starts("фут") || starts("feet") || starts("foot") {
        u(Kind::Length, 0.3048, "фут.", "м")
    } else if starts("ярд") || starts("yard") {
        u(Kind::Length, 0.9144, "ярд.", "м")
    } else if starts("мил") || starts("mile") {
        u(Kind::Length, 1609.344, "миль", "км")
    } else if starts("миллиграмм") {
        u(Kind::Mass, 1e-6, "мг", "г")
    } else if starts("килограмм") || starts("кило") {
        u(Kind::Mass, 1.0, "кг", "фунт")
    } else if starts("грамм") {
        u(Kind::Mass, 0.001, "г", "унция")
    } else if starts("тонн") {
        u(Kind::Mass, 1000.0, "т", "кг")
    } else if starts("фунт") || starts("pound") {
        u(Kind::Mass, 0.453_592_37, "фунт.", "кг")
    } else if starts("унц") || starts("ounce") {
        u(Kind::Mass, 0.028_349_523, "унц.", "г")
    } else if starts("миллилитр") {
        u(Kind::Volume, 0.001, "мл", "л")
    } else if starts("литр") {
        u(Kind::Volume, 1.0, "л", "галлон")
    } else if starts("галлон") || starts("gallon") {
        u(Kind::Volume, 3.785_411_784, "галл.", "л")
    } else if starts("байт") {
        u(Kind::Data, 1.0, "байт", "кб")
    } else if starts("килобайт") {
        u(Kind::Data, 1024.0, "КБ", "мб")
    } else if starts("мегабайт") {
        u(Kind::Data, 1024.0 * 1024.0, "МБ", "гб")
    } else if starts("гигабайт") {
        u(Kind::Data, 1024.0 * 1024.0 * 1024.0, "ГБ", "мб")
    } else if starts("терабайт") {
        u(Kind::Data, 1024.0f64.powi(4), "ТБ", "гб")
    } else if starts("секунд") {
        u(Kind::Time, 1.0, "с", "мин")
    } else if starts("минут") {
        u(Kind::Time, 60.0, "мин", "ч")
    } else if starts("час") {
        u(Kind::Time, 3600.0, "ч", "мин")
    } else if starts("день") || starts("дня") || starts("дней") || starts("сут") {
        u(Kind::Time, 86400.0, "дн.", "ч")
    } else if starts("недел") {
        u(Kind::Time, 604_800.0, "нед.", "дн")
    } else if starts("цельси") || starts("celsius") || starts("градус") {
        Some(Unit { kind: Kind::Temp, k: 1.0, label: "°C", code: "c", default: "f" })
    } else if starts("фаренгейт") || starts("fahrenheit") {
        Some(Unit { kind: Kind::Temp, k: 1.0, label: "°F", code: "f", default: "c" })
    } else if starts("кельвин") || starts("kelvin") {
        Some(Unit { kind: Kind::Temp, k: 1.0, label: "K", code: "k", default: "c" })
    } else {
        None
    };
    named.or_else(|| money(&w))
}

fn money(w: &str) -> Option<Unit> {
    let code = match w {
        "$" | "usd" | "бакс" | "баксов" | "бакса" | "баксы" => "USD",
        "€" | "eur" | "евро" => "EUR",
        "₽" | "rub" | "руб" | "р" => "RUB",
        "₴" | "uah" | "грн" => "UAH",
        "£" | "gbp" => "GBP",
        "¥" | "jpy" => "JPY",
        "₸" | "kzt" | "тг" => "KZT",
        "byn" => "BYN",
        "cny" => "CNY",
        "pln" | "zł" => "PLN",
        "try" | "₺" => "TRY",
        "chf" => "CHF",
        "czk" => "CZK",
        "gel" => "GEL",
        "amd" => "AMD",
        _ if w.starts_with("доллар") => "USD",
        _ if w.starts_with("рубл") => "RUB",
        _ if w.starts_with("грив") => "UAH",
        _ if w.starts_with("тенге") => "KZT",
        _ if w.starts_with("юан") => "CNY",
        _ if w.starts_with("злот") => "PLN",
        _ if w.starts_with("лир") => "TRY",
        _ if w.starts_with("иен") || w.starts_with("йен") => "JPY",
        _ if w.starts_with("франк") => "CHF",
        _ if w.starts_with("крон") => "CZK",
        _ if w.starts_with("лари") => "GEL",
        _ if w.starts_with("драм") => "AMD",
        _ => return None,
    };
    Some(Unit { kind: Kind::Money, k: 1.0, label: code, code, default: "" })
}

/// Спрашивают ли про валюту — тогда нужны курсы.
pub fn wants_rates(q: &str) -> bool {
    parse(q).is_some_and(|(_, from, to)| from.kind == Kind::Money || to.is_some_and(|t| t.kind == Kind::Money))
}

/// Разбирает «число единица [в|to|in|=] единица». Число может быть слитно с единицей («5км», «$100»).
fn parse(q: &str) -> Option<(f64, Unit, Option<Unit>)> {
    let q = q.trim().to_lowercase();
    // знак валюты перед числом: «$100» → «100 $»
    let q = match q.chars().next() {
        Some(c) if "$€₽₴£¥₸₺".contains(c) => {
            let rest = &q[c.len_utf8()..];
            let cut = rest.char_indices().find(|(_, ch)| !(ch.is_ascii_digit() || ".,".contains(*ch))).map_or(rest.len(), |(i, _)| i);
            format!("{} {c} {}", &rest[..cut], &rest[cut..])
        }
        _ => q,
    };
    let end = q.char_indices().find(|(_, c)| !(c.is_ascii_digit() || " .,-".contains(*c))).map(|(i, _)| i)?;
    let num: String = q[..end].chars().filter(|c| !c.is_whitespace()).collect();
    if num.is_empty() || !num.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    // запятая — десятичная; точка тоже, если она одна
    let value: f64 = num.replace(',', ".").parse().ok()?;
    let rest = q[end..].trim();
    let mut parts = rest.split_whitespace();
    let from = unit(parts.next()?)?;
    let mut next = parts.next();
    if matches!(next, Some("в" | "во" | "to" | "in" | "=" | "->" | "→")) {
        next = parts.next();
    }
    let to = match next {
        Some(w) => Some(unit(w)?),
        None => None,
    };
    if parts.next().is_some() {
        return None;
    }
    Some((value, from, to))
}

fn to_celsius(v: f64, code: &str) -> f64 {
    match code {
        "f" => (v - 32.0) / 1.8,
        "k" => v - 273.15,
        _ => v,
    }
}

fn from_celsius(c: f64, code: &str) -> f64 {
    match code {
        "f" => c * 1.8 + 32.0,
        "k" => c + 273.15,
        _ => c,
    }
}

/// Число по-русски: до четырёх значащих цифр после запятой, разряды через пробел.
pub fn fmt(v: f64) -> String {
    let a = v.abs();
    let digits = if a >= 100.0 || a == 0.0 { 2 } else if a >= 1.0 { 3 } else { (3 - a.log10().floor() as i32).clamp(3, 8) as usize };
    let s = format!("{v:.digits$}");
    let s = if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s };
    let (int, frac) = s.split_once('.').map_or((s.as_str(), ""), |(i, f)| (i, f));
    let (sign, int) = int.strip_prefix('-').map_or(("", int), |i| ("-", i));
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            out.push('\u{00A0}');
        }
        out.push(c);
    }
    if frac.is_empty() { format!("{sign}{out}") } else { format!("{sign}{out},{frac}") }
}

/// Результаты для строки поиска: что показать и что положить в буфер по нажатию.
/// rates — курсы к доллару; None — ещё не загружены.
pub fn convert(q: &str, rates: Option<&HashMap<String, f64>>) -> Vec<(String, String, &'static str)> {
    let Some((value, from, to)) = parse(q) else { return Vec::new() };
    let mut out = Vec::new();
    if from.kind == Kind::Money {
        let Some(rates) = rates else {
            return vec![("Курс валют загружается…".into(), String::new(), "валюта")];
        };
        let Some(base) = rates.get(from.code).filter(|r| **r > 0.0) else { return out };
        // цель не названа — показываем основные валюты
        let targets: Vec<&str> = match &to {
            Some(t) if t.kind == Kind::Money => vec![t.code],
            Some(_) => return out,
            None => ["USD", "EUR", "RUB", "UAH"].into_iter().filter(|c| *c != from.code).take(3).collect(),
        };
        for code in targets {
            if let Some(r) = rates.get(code) {
                // деньги — до копеек
                let v = fmt((value / base * r * 100.0).round() / 100.0);
                out.push((format!("{} {} = {v} {code}", fmt(value), from.code), v, "курс: exchangerate-api.com"));
            }
        }
        return out;
    }
    let target = match to {
        Some(t) => t,
        None => match unit(from.default) {
            Some(t) => t,
            None => return out,
        },
    };
    if target.kind != from.kind {
        return out;
    }
    let v = if from.kind == Kind::Temp { from_celsius(to_celsius(value, from.code), target.code) } else { value * from.k / target.k };
    let v = fmt(v);
    out.push((format!("{} {} = {v} {}", fmt(value), from.label, target.label), v, "скопировать"));
    out
}

/// Курсы валют к доллару: в памяти и в файле рядом с настройками, чтобы работать без сети.
#[derive(Clone, Default)]
pub struct Rates {
    state: Arc<Mutex<(Option<HashMap<String, f64>>, Option<Instant>, bool)>>,
}

impl Rates {
    pub fn get(&self) -> Option<HashMap<String, f64>> {
        self.state.lock().unwrap().0.clone()
    }

    /// Подгружает курсы, если их нет или они старше полусуток. done вызывается, когда пришли новые.
    pub fn refresh(&self, cache: std::path::PathBuf, done: impl Fn() + Send + 'static) {
        {
            let mut st = self.state.lock().unwrap();
            let fresh = st.1.is_some_and(|t| t.elapsed() < Duration::from_secs(12 * 3600));
            if st.2 || fresh {
                return;
            }
            st.2 = true;
        }
        let me = self.clone();
        std::thread::spawn(move || {
            let parse = |text: &str| -> Option<HashMap<String, f64>> {
                let v: serde_json::Value = serde_json::from_str(text).ok()?;
                let map: HashMap<String, f64> = v["rates"].as_object()?.iter().filter_map(|(k, r)| Some((k.clone(), r.as_f64()?))).collect();
                (map.len() > 20).then_some(map)
            };
            // пока идёт запрос — показываем то, что сохранили в прошлый раз
            if me.get().is_none() {
                if let Some(old) = std::fs::read_to_string(&cache).ok().and_then(|t| parse(&t)) {
                    me.state.lock().unwrap().0 = Some(old);
                    done();
                }
            }
            let fetched = ureq::get("https://open.er-api.com/v6/latest/USD").timeout(Duration::from_secs(12)).call().ok().and_then(|r| r.into_string().ok());
            let mut st = me.state.lock().unwrap();
            st.2 = false;
            if let Some((text, map)) = fetched.and_then(|t| parse(&t).map(|m| (t, m))) {
                let _ = std::fs::write(&cache, text);
                st.0 = Some(map);
                st.1 = Some(Instant::now());
            } else {
                // не вышло — повторим не раньше чем через пять минут
                st.1 = Some(Instant::now() - Duration::from_secs(12 * 3600 - 300));
            }
            drop(st);
            done();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(q: &str) -> String {
        convert(q, None).into_iter().next().map(|r| r.0).unwrap_or_default()
    }

    #[test]
    fn units() {
        assert_eq!(one("5 км в мили"), "5 км = 3,107 миль");
        assert_eq!(one("5км"), "5 км = 3,107 миль");
        assert_eq!(one("10 дюймов в см"), "10 дюйм. = 25,4 см");
        assert_eq!(one("2,5 кг в фунты"), "2,5 кг = 5,512 фунт.");
        assert_eq!(one("1 гб в мб"), "1 ГБ = 1\u{00A0}024 МБ");
        assert_eq!(one("90 минут в часы"), "90 мин = 1,5 ч");
        assert_eq!(one("100 км/ч в м/с"), "100 км/ч = 27,778 м/с");
        assert_eq!(one("1 000 м в км"), "1\u{00A0}000 м = 1 км");
    }

    #[test]
    fn temperature() {
        assert_eq!(one("72 f в c"), "72 °F = 22,222 °C");
        assert_eq!(one("0 c"), "0 °C = 32 °F");
        assert_eq!(one("-40 c в f"), "-40 °C = -40 °F");
        assert_eq!(one("300 кельвинов в цельсии"), "300 K = 26,85 °C");
    }

    #[test]
    fn money() {
        let rates: HashMap<String, f64> = [("USD", 1.0), ("EUR", 0.9), ("RUB", 80.0), ("UAH", 40.0)].into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        let r = convert("100 usd в рубли", Some(&rates));
        assert_eq!(r[0].0, "100 USD = 8\u{00A0}000 RUB");
        assert_eq!(r[0].1, "8\u{00A0}000");
        assert_eq!(convert("$50 в евро", Some(&rates))[0].0, "50 USD = 45 EUR");
        assert_eq!(convert("1000 рублей в доллары", Some(&rates))[0].0, "1\u{00A0}000 RUB = 12,5 USD");
        // цель не названа — три основные валюты
        assert_eq!(convert("10 евро", Some(&rates)).len(), 3);
        assert!(wants_rates("5 долларов") && !wants_rates("5 км"));
        assert_eq!(convert("5 usd", None)[0].0, "Курс валют загружается…");
    }

    #[test]
    fn not_a_conversion() {
        for q in ["блокнот", "5", "5 км в кг", "2 + 2", "таймер 5", "youtube.com", "5 яблок"] {
            assert!(convert(q, None).is_empty(), "{q}");
        }
    }
}
