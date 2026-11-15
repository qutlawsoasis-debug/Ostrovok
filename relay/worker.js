// Посредник для Джарвиса: принимает вопросы от островов и пересылает их в сервис с большой моделью,
// подставляя свой ключ. Пользователям острова ключ не нужен и не виден.
// Работает как Cloudflare Worker (бесплатный тариф, HTTPS из коробки).
//
// Секреты и настройки (задаются в Cloudflare, в коде их нет):
//   API_KEY  — ключ сервиса (секрет)
//   MODEL    — имя модели в сервисе
//   UPSTREAM — адрес сервиса, по умолчанию Groq
//   LIMITER  — необязательная привязка Rate Limiting: ограничение запросов с одного адреса

const MAX_BODY = 60_000; // символов: переписка Джарвиса короткая, большие запросы — не наши
const MAX_TOKENS = 800;

const reply = (status, message) =>
  new Response(JSON.stringify({ error: { message } }), { status, headers: { "content-type": "application/json" } });

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (request.method !== "POST" || !url.pathname.endsWith("/chat/completions")) {
      return reply(404, "not found");
    }
    if (!env.API_KEY || !env.MODEL) {
      return reply(500, "relay is not configured");
    }

    // не больше заданного числа запросов в минуту с одного адреса
    if (env.LIMITER) {
      const ip = request.headers.get("cf-connecting-ip") || "unknown";
      const { success } = await env.LIMITER.limit({ key: ip });
      if (!success) {
        return reply(429, "too many requests");
      }
    }

    const text = await request.text();
    if (text.length > MAX_BODY) {
      return reply(413, "request too large");
    }
    let body;
    try {
      body = JSON.parse(text);
    } catch {
      return reply(400, "bad json");
    }
    if (!Array.isArray(body.messages)) {
      return reply(400, "messages required");
    }

    // модель и пределы задаёт посредник, а не тот, кто прислал запрос
    const upstream = {
      model: env.MODEL,
      messages: body.messages,
      tools: Array.isArray(body.tools) ? body.tools : undefined,
      temperature: typeof body.temperature === "number" ? body.temperature : 0.4,
      max_tokens: Math.min(Number(body.max_tokens) || 600, MAX_TOKENS),
    };

    const answer = await fetch(env.UPSTREAM || "https://api.groq.com/openai/v1/chat/completions", {
      method: "POST",
      headers: { authorization: `Bearer ${env.API_KEY}`, "content-type": "application/json" },
      body: JSON.stringify(upstream),
    });
    return new Response(answer.body, { status: answer.status, headers: { "content-type": "application/json" } });
  },
};
