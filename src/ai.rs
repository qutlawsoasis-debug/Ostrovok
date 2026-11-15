//! Встроенный ИИ: запросы к Claude API (Anthropic) по обычному HTTPS.
//! Ключ API вводит сам пользователь; наружу уходит только текст переписки из вкладки «ИИ».

use serde_json::{json, Value};

const MODEL: &str = "claude-opus-5-5";
const SYSTEM: &str = "Ты — помощник внутри маленькой панели «Островок» на рабочем столе Windows. \
Место на экране ограничено, поэтому отвечай кратко и по делу: обычно хватает нескольких предложений. \
Пиши обычным текстом без разметки Markdown, потому что панель её не отображает. Отвечай на языке вопроса.";

/// history: пары (сообщение пользователя?, текст) по порядку; последняя — новый вопрос.
pub fn ask(key: &str, history: &[(bool, String)]) -> Result<String, String> {
    let messages: Vec<Value> = history
        .iter()
        .map(|(me, text)| json!({ "role": if *me { "user" } else { "assistant" }, "content": text }))
        .collect();
    let body = json!({
        "model": MODEL,
        "max_tokens": 16000,
        "system": SYSTEM,
        // низкое усилие: в панели важнее быстрый ответ
        "output_config": { "effort": "low" },
        // если защитный фильтр отклонит запрос, сервер сам повторит его на запасной модели
        "fallbacks": "default",
        "messages": messages,
    });

    let resp = ureq::post("https://api.anthropic.com/v1/messages")
        .timeout(std::time::Duration::from_secs(180))
        .set("content-type", "application/json")
        .set("x-api-key", key)
        .set("anthropic-version", "2026-10-21")
        .set("anthropic-beta", "server-side-fallback-2026-07-01")
        .send_json(body);

    let v: Value = match resp {
        Ok(r) => r.into_json().map_err(|e| format!("Не удалось прочитать ответ: {e}"))?,
        Err(ureq::Error::Status(code, r)) => {
            let detail = r
                .into_json::<Value>()
                .ok()
                .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
                .unwrap_or_default();
            return Err(match code {
                401 | 403 => "Ключ API не подошёл. Проверьте его в настройках.".to_string(),
                429 => "Слишком много запросов или закончился лимит. Попробуйте позже.".to_string(),
                500..=599 => "Сервис временно недоступен. Попробуйте ещё раз.".to_string(),
                _ => format!("Ошибка {code}: {detail}"),
            });
        }
        Err(_) => return Err("Нет связи с api.anthropic.com.".to_string()),
    };

    if v["stop_reason"].as_str() == Some("refusal") {
        return Err("Модель отказалась отвечать на этот вопрос.".to_string());
    }
    let text: String = v["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"].as_str() == Some("text"))
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(if v["stop_reason"].as_str() == Some("max_tokens") {
            "Ответ не поместился. Попробуйте спросить короче.".to_string()
        } else {
            "Пустой ответ.".to_string()
        });
    }
    Ok(text)
}
