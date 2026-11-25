//! Бесплатные игры: раздачи Steam (игры со скидкой 100% — остаются в библиотеке навсегда) и Epic Games.
//! Оба источника — официальные открытые адреса магазинов, без ключей и входа. Steam важнее и идёт первым.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub enum Store {
    Steam,
    Epic,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Game {
    pub store: Store,
    /// Ключ для «уже видели»: «steam:1222140», «epic:mark-of-the-deep-f7dc08».
    pub id: String,
    pub title: String,
    /// Страница в магазине (для Steam — https; клиент открывается через steam://store/<id>).
    pub url: String,
    /// Steam: номер приложения.
    pub appid: Option<u32>,
    /// До какого момента раздают (у Steam поиск этого не сообщает).
    pub ends: Option<chrono::DateTime<chrono::Utc>>,
}

impl Game {
    pub fn store_name(&self) -> &'static str {
        match self.store {
            Store::Steam => "Steam",
            Store::Epic => "Epic Games",
        }
    }
}

const STEAM_URL: &str = "https://store.steampowered.com/search/results/?query=&maxprice=free&specials=1&category1=998&json=1&cc=ru&l=russian";
const EPIC_URL: &str = "https://store-site-backend-static-ipv4.ak.epicgames.com/freeGamesPromotions?locale=ru&country=RU";

fn get(url: &str) -> Result<Value, String> {
    ureq::get(url)
        .timeout(std::time::Duration::from_secs(12))
        .call()
        .map_err(|e| e.to_string())?
        .into_json::<Value>()
        .map_err(|e| e.to_string())
}

/// Всё, что раздают сейчас: сначала Steam, потом Epic. Ошибка — только если не ответил ни один магазин.
pub fn fetch() -> Result<Vec<Game>, String> {
    let now = chrono::Utc::now();
    let steam = get(STEAM_URL).map(|v| parse_steam(&v));
    let epic = get(EPIC_URL).map(|v| parse_epic(&v, now));
    match (steam, epic) {
        (Err(a), Err(_)) => Err(a),
        (s, e) => {
            let mut out = s.unwrap_or_default();
            out.extend(e.unwrap_or_default());
            Ok(out)
        }
    }
}

/// Номер приложения из адреса картинки: …/steam/apps/1222140/capsule_sm_120.jpg
fn steam_appid(logo: &str) -> Option<u32> {
    let rest = &logo[logo.find("/apps/")? + 6..];
    rest[..rest.find('/')?].parse().ok()
}

pub fn parse_steam(v: &Value) -> Vec<Game> {
    let mut out = Vec::new();
    for it in v["items"].as_array().into_iter().flatten() {
        let (Some(name), Some(id)) = (it["name"].as_str(), it["logo"].as_str().and_then(steam_appid)) else { continue };
        let name = name.trim();
        if name.is_empty() || out.iter().any(|g: &Game| g.appid == Some(id)) {
            continue;
        }
        out.push(Game {
            store: Store::Steam,
            id: format!("steam:{id}"),
            title: name.to_string(),
            url: format!("https://store.steampowered.com/app/{id}"),
            appid: Some(id),
            ends: None,
        });
        if out.len() >= 5 {
            break;
        }
    }
    out
}

fn ts(v: &Value) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(v.as_str()?).ok().map(|d| d.with_timezone(&chrono::Utc))
}

/// Идущая сейчас раздача (скидка до нуля): возвращает её конец.
fn epic_live_until(el: &Value, now: chrono::DateTime<chrono::Utc>) -> Option<chrono::DateTime<chrono::Utc>> {
    let groups = el["promotions"]["promotionalOffers"].as_array()?;
    for o in groups.iter().filter_map(|g| g["promotionalOffers"].as_array()).flatten() {
        let free = o["discountSetting"]["discountPercentage"].as_i64() == Some(0);
        let (Some(start), Some(end)) = (ts(&o["startDate"]), ts(&o["endDate"])) else { continue };
        if free && start <= now && now < end {
            return Some(end);
        }
    }
    None
}

fn epic_slug(el: &Value) -> Option<String> {
    let mapped = |key: &str| {
        el[key].as_array().into_iter().flatten().find_map(|m| m["pageSlug"].as_str().filter(|s| !s.is_empty()).map(str::to_string))
    };
    el["catalogNs"]["mappings"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|m| m["pageSlug"].as_str().filter(|s| !s.is_empty()).map(str::to_string))
        .or_else(|| mapped("offerMappings"))
        .or_else(|| el["productSlug"].as_str().filter(|s| !s.is_empty()).map(|s| s.trim_end_matches("/home").to_string()))
}

pub fn parse_epic(v: &Value, now: chrono::DateTime<chrono::Utc>) -> Vec<Game> {
    let mut out = Vec::new();
    for el in v["data"]["Catalog"]["searchStore"]["elements"].as_array().into_iter().flatten() {
        let Some(end) = epic_live_until(el, now) else { continue };
        // «бесплатные навсегда» (free-to-play) тоже попадают в список с нулевой ценой — нам нужны только раздачи
        if el["price"]["totalPrice"]["originalPrice"].as_i64() == Some(0) {
            continue;
        }
        let (Some(title), Some(slug)) = (el["title"].as_str(), epic_slug(el)) else { continue };
        if title.trim().is_empty() || title.trim().eq_ignore_ascii_case("mystery game") {
            continue;
        }
        out.push(Game {
            store: Store::Epic,
            id: format!("epic:{slug}"),
            title: title.trim().to_string(),
            url: format!("https://store.epicgames.com/ru/p/{slug}"),
            appid: None,
            ends: Some(end),
        });
    }
    out
}

/// «ещё 2 дня», «ещё 5 ч», «до конца меньше часа»; без даты — пусто.
pub fn left_text(ends: Option<chrono::DateTime<chrono::Utc>>, now: chrono::DateTime<chrono::Utc>) -> String {
    let Some(end) = ends else { return String::new() };
    let mins = (end - now).num_minutes();
    if mins <= 0 {
        String::new()
    } else if mins < 60 {
        "меньше часа".into()
    } else if mins < 48 * 60 {
        format!("ещё {} ч", mins / 60)
    } else {
        let d = mins / (24 * 60);
        let word = match (d % 10, d % 100) {
            (1, n) if n != 11 => "день",
            (2..=4, n) if !(12..=14).contains(&n) => "дня",
            _ => "дней",
        };
        format!("ещё {d} {word}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc)
    }

    #[test]
    fn steam_items() {
        let v = json!({"desc":"","items":[
            {"name":"Detroit: Become Human","logo":"https://shared.fastly.steamstatic.com/store_item_assets/steam/apps/1222140/capsule_sm_120.jpg?t=1"},
            {"name":"Baldur's Gate 3","logo":"https://x/steam/apps/1086940/3dd81008/capsule_sm_120.jpg"},
            {"name":"Без картинки"},
            {"name":"Дубль","logo":"https://x/steam/apps/1222140/a.jpg"}
        ]});
        let g = parse_steam(&v);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].appid, Some(1222140));
        assert_eq!(g[0].id, "steam:1222140");
        assert_eq!(g[0].url, "https://store.steampowered.com/app/1222140");
        assert_eq!(g[1].title, "Baldur's Gate 3");
        assert!(parse_steam(&json!({"desc":"","items":[]})).is_empty());
        assert!(parse_steam(&json!({"oops":1})).is_empty());
    }

    fn epic_el(title: &str, slug: &str, original: i64, start: &str, end: &str, pct: i64) -> Value {
        json!({
            "title": title, "productSlug": null,
            "catalogNs": {"mappings": [{"pageSlug": slug, "pageType": "productHome"}]},
            "price": {"totalPrice": {"discountPrice": 0, "originalPrice": original}},
            "promotions": {"promotionalOffers": [{"promotionalOffers": [
                {"startDate": start, "endDate": end, "discountSetting": {"discountType": "PERCENTAGE", "discountPercentage": pct}}
            ]}], "upcomingPromotionalOffers": []}
        })
    }

    #[test]
    fn epic_only_live_giveaways() {
        let now = at("2026-11-25T12:00:00Z");
        let v = json!({"data":{"Catalog":{"searchStore":{"elements":[
            epic_el("Mark of the Deep", "mark-of-the-deep-f7dc08", 59900, "2026-11-19T16:00:00.000Z", "2026-11-26T16:00:00.000Z", 0),
            // скидка, а не раздача
            epic_el("Ghostrunner 2", "ghostrunner-2", 284900, "2026-11-19T16:00:00.000Z", "2026-12-01T15:00:00.000Z", 20),
            // раздача ещё не началась
            epic_el("Скоро", "soon", 32000, "2026-11-26T16:00:00.000Z", "2026-12-03T16:00:00.000Z", 0),
            // бесплатна всегда
            epic_el("Free to play", "f2p", 0, "2026-11-19T16:00:00.000Z", "2026-12-03T16:00:00.000Z", 0),
            {"title":"Без акций","promotions":null}
        ]}}}});
        let g = parse_epic(&v, now);
        assert_eq!(g.len(), 1, "{g:?}");
        assert_eq!(g[0].title, "Mark of the Deep");
        assert_eq!(g[0].url, "https://store.epicgames.com/ru/p/mark-of-the-deep-f7dc08");
        assert_eq!(g[0].ends, Some(at("2026-11-26T16:00:00Z")));
        assert_eq!(left_text(g[0].ends, now), "ещё 28 ч");
    }

    #[test]
    fn epic_slug_fallbacks() {
        assert_eq!(epic_slug(&json!({"productSlug":"thems-fightin-herds/home"})).as_deref(), Some("thems-fightin-herds"));
        assert_eq!(epic_slug(&json!({"offerMappings":[{"pageSlug":"abc"}],"productSlug":"zzz"})).as_deref(), Some("abc"));
        assert_eq!(epic_slug(&json!({"catalogNs":{"mappings":[]},"productSlug":""})), None);
    }

    #[test]
    fn left_words() {
        let now = at("2026-11-25T12:00:00Z");
        assert_eq!(left_text(None, now), "");
        assert_eq!(left_text(Some(at("2026-11-25T11:00:00Z")), now), "");
        assert_eq!(left_text(Some(at("2026-11-25T12:30:00Z")), now), "меньше часа");
        assert_eq!(left_text(Some(at("2026-11-27T13:00:00Z")), now), "ещё 2 дня");
        assert_eq!(left_text(Some(at("2026-11-30T13:00:00Z")), now), "ещё 5 дней");
        assert_eq!(left_text(Some(at("2026-12-16T13:00:00Z")), now), "ещё 21 день");
        assert_eq!(left_text(Some(at("2026-12-06T13:00:00Z")), now), "ещё 11 дней");
    }
}

#[cfg(test)]
mod live {
    /// Живая проверка магазинов: cargo test --release free::live -- --ignored --nocapture
    #[test]
    #[ignore]
    fn fetch_now() {
        let g = super::fetch().expect("магазины не ответили");
        for x in &g {
            println!("{:?} {} {} {:?}", x.store, x.title, x.url, x.ends);
        }
    }
}
