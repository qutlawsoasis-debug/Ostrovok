// «Что нового»: все релизы Островка с GitHub, поиск и группировка по дням.
// Всё из описаний релизов вставляется только как текст (textContent / createTextNode).
(function (root) {
  'use strict';

  var API = 'https://api.github.com/repos/qutlawsoasis-debug/Ostrovok/releases';
  var RELEASES = 'https://github.com/qutlawsoasis-debug/Ostrovok/releases';
  var CACHE_KEY = 'ostrovok.releases';
  var FALLBACK = 'Исправления и улучшения';
  var NO_DATE = 'Без даты';
  var VERSIONS = ['версия', 'версии', 'версий'];
  var MONTHS_GEN = ['января', 'февраля', 'марта', 'апреля', 'мая', 'июня',
    'июля', 'августа', 'сентября', 'октября', 'ноября', 'декабря'];

  var pad2 = function (n) { return (n < 10 ? '0' : '') + n; };

  function toDate(v) {
    if (v === null || v === undefined || v === '') return null;
    var d = v instanceof Date ? v : new Date(v);
    return isNaN(d.getTime()) ? null : d;
  }

  // ── даты (по местному времени читателя)
  function dayKey(iso) {
    var d = toDate(iso);
    return d ? d.getFullYear() + '-' + pad2(d.getMonth() + 1) + '-' + pad2(d.getDate()) : null;
  }

  function formatDayRu(key) {
    var m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(key || '');
    if (!m || +m[2] < 1 || +m[2] > 12) return NO_DATE;
    return +m[3] + ' ' + MONTHS_GEN[+m[2] - 1] + ' ' + m[1];
  }

  function formatTime(v) {
    var d = toDate(v);
    return d ? pad2(d.getHours()) + ':' + pad2(d.getMinutes()) : '';
  }

  function pluralRu(n, forms) {
    var a = Math.abs(n) % 100, b = a % 10;
    if (a > 10 && a < 20) return forms[2];
    if (b === 1) return forms[0];
    if (b > 1 && b < 5) return forms[1];
    return forms[2];
  }

  function releaseDate(r) {
    return (r && (r.published_at || r.created_at)) || null;
  }

  // ── подготовка и поиск
  function normalize(s) {
    if (s === null || s === undefined) return '';
    return String(s).toLowerCase().replace(/ё/g, 'е').replace(/\s+/g, ' ').trim();
  }

  function prepare(list) {
    if (!Array.isArray(list)) return [];
    var out = [];
    for (var i = 0; i < list.length; i++) {
      var r = list[i];
      if (!r || typeof r !== 'object' || r.draft) continue;
      var copy = {};
      for (var k in r) if (Object.prototype.hasOwnProperty.call(r, k)) copy[k] = r[k];
      var d = toDate(releaseDate(r));
      copy._time = d ? d.getTime() : null;
      copy._search = normalize((r.tag_name || '') + ' ' + (r.name || '') + ' ' + (r.body || ''));
      copy._i = out.length;
      out.push(copy);
    }
    out.sort(function (a, b) {
      if (a._time === null || b._time === null) {
        if (a._time === b._time) return a._i - b._i;
        return a._time === null ? 1 : -1;
      }
      return b._time - a._time || a._i - b._i;
    });
    return out;
  }

  function matches(r, query) {
    var q = normalize(query);
    if (!q) return true;
    var words = q.split(' ');
    for (var i = 0; i < words.length; i++) if (r._search.indexOf(words[i]) < 0) return false;
    return true;
  }

  function groupByDay(releases) {
    var groups = [], byKey = {}, none = null;
    for (var i = 0; i < releases.length; i++) {
      var r = releases[i], key = dayKey(releaseDate(r));
      var g = key ? byKey[key] : none;
      if (!g) {
        g = { key: key, title: formatDayRu(key), items: [] };
        if (key) { byKey[key] = g; groups.push(g); } else none = g;
      }
      g.items.push(r);
    }
    groups.sort(function (a, b) { return a.key < b.key ? 1 : a.key > b.key ? -1 : 0; });
    if (none) groups.push(none);
    return groups;
  }

  // ── разбор описания релиза
  function parseInline(text) {
    var s = text === null || text === undefined ? '' : String(text);
    var out = [], buf = '', i = 0;
    var flush = function () { if (buf) { out.push({ t: 'text', v: buf }); buf = ''; } };
    while (i < s.length) {
      if (s.charAt(i) === '*' && s.charAt(i + 1) === '*') {
        var e = s.indexOf('**', i + 2);
        if (e > i + 2) { flush(); out.push({ t: 'strong', v: s.slice(i + 2, e) }); i = e + 2; continue; }
      } else if (s.charAt(i) === '`') {
        var c = s.indexOf('`', i + 1);
        if (c > i + 1) { flush(); out.push({ t: 'code', v: s.slice(i + 1, c) }); i = c + 1; continue; }
      }
      buf += s.charAt(i); i++;
    }
    flush();
    return out;
  }

  function parseBody(body) {
    var lines = String(body === null || body === undefined ? '' : body).replace(/\r\n?/g, '\n').split('\n');
    var blocks = [], para = null, list = null;
    var end = function () {
      if (para) blocks.push({ type: 'p', inline: parseInline(para.join('\n')) });
      if (list) blocks.push({ type: 'ul', items: list });
      para = null; list = null;
    };
    for (var i = 0; i < lines.length; i++) {
      var line = lines[i].trim(), m;
      if (!line) { end(); continue; }
      if ((m = /^#{1,3}\s+(.*)$/.exec(line))) { end(); blocks.push({ type: 'h', text: m[1].trim() }); continue; }
      if ((m = /^[-*•]\s+(.*)$/.exec(line))) {
        if (para) end();
        (list = list || []).push(parseInline(m[1]));
        continue;
      }
      if (list) end();
      (para = para || []).push(line);
    }
    end();
    return blocks.length ? blocks : [{ type: 'p', inline: [{ t: 'text', v: FALLBACK }] }];
  }

  function safeHref(u) {
    return typeof u === 'string' && u.indexOf('https://github.com/') === 0 ? u : RELEASES;
  }

  // ── ошибки загрузки
  function classifyError(err, res, opts) {
    opts = opts || {};
    var online = opts.online;
    if (online === undefined && typeof navigator !== 'undefined') online = navigator.onLine;
    if (online === false || err instanceof TypeError) {
      return { kind: 'offline', message: 'Нет связи с интернетом. Проверьте подключение и обновите страницу.' };
    }
    if (res && !res.ok) {
      var h = function (k) { try { return res.headers ? res.headers.get(k) : null; } catch (e) { return null; } };
      if (res.status === 429 || (res.status === 403 && h('X-RateLimit-Remaining') === '0')) {
        var now = opts.now === undefined ? Date.now() : opts.now, at = null;
        var reset = Number(h('X-RateLimit-Reset')), retry = Number(h('Retry-After'));
        if (h('X-RateLimit-Reset') && reset > 0) at = reset * 1000;
        else if (h('Retry-After') && retry >= 0) at = now + retry * 1000;
        return {
          kind: 'rate',
          message: 'GitHub временно ограничил запросы с вашего адреса. Попробуйте ' +
            (at ? 'после ' + formatTime(at) + '.' : 'через несколько минут.')
        };
      }
      return { kind: 'http', message: 'GitHub ответил ошибкой ' + res.status + '. Попробуйте позже.' };
    }
    return { kind: 'parse', message: 'Не удалось прочитать ответ GitHub.' };
  }

  function loadError(info) {
    var e = new Error(info.message);
    e.kind = info.kind;
    return e;
  }

  async function fetchPage(fetchFn, url) {
    var res;
    try { res = await fetchFn(url, { headers: { Accept: 'application/vnd.github+json' } }); }
    catch (e) { throw loadError(classifyError(e)); }
    if (!res || !res.ok) throw loadError(classifyError(null, res));
    var data;
    try { data = await res.json(); } catch (e) { throw loadError(classifyError(e)); }
    if (!Array.isArray(data)) throw loadError(classifyError(null, null));
    return data;
  }

  async function fetchAll(fetchFn, opts) {
    opts = opts || {};
    var perPage = opts.perPage || 100, maxPages = opts.maxPages || 20;
    var releases = [];
    for (var p = 1; p <= maxPages; p++) {
      var data;
      try { data = await fetchPage(fetchFn, API + '?per_page=' + perPage + '&page=' + p); }
      catch (e) {
        if (p === 1) throw e;
        return { releases: releases, partial: { kind: e.kind, message: 'Показаны не все версии: ' + e.message } };
      }
      for (var i = 0; i < data.length; i++) releases.push(data[i]);
      if (data.length < perPage) break;
    }
    return { releases: releases, partial: null };
  }

  // ── кэш в localStorage (может быть запрещён — тогда просто без кэша)
  var CACHE_FIELDS = ['tag_name', 'name', 'published_at', 'created_at', 'body', 'html_url', 'draft', 'prerelease'];

  function saveCache(storage, releases, now) {
    try {
      if (!storage) return false;
      var slim = [];
      for (var i = 0; i < releases.length; i++) {
        var r = releases[i], o = {};
        for (var j = 0; j < CACHE_FIELDS.length; j++) if (r[CACHE_FIELDS[j]] !== undefined) o[CACHE_FIELDS[j]] = r[CACHE_FIELDS[j]];
        slim.push(o);
      }
      storage.setItem(CACHE_KEY, JSON.stringify({ savedAt: now, releases: slim }));
      return true;
    } catch (e) { return false; }
  }

  function loadCache(storage) {
    try {
      if (!storage) return null;
      var v = JSON.parse(storage.getItem(CACHE_KEY));
      if (!v || typeof v.savedAt !== 'number' || !Array.isArray(v.releases)) return null;
      return { savedAt: v.savedAt, releases: v.releases };
    } catch (e) { return null; }
  }

  function cacheNote(savedAt) {
    return 'Показаны сохранённые данные от ' + formatDayRu(dayKey(savedAt)) + ', ' + formatTime(savedAt);
  }

  // ── DOM: только createElement / textContent
  function el(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text) e.textContent = text;
    return e;
  }

  function renderInline(parent, parts) {
    for (var i = 0; i < parts.length; i++) {
      var p = parts[i];
      if (p.t === 'strong') parent.append(el('strong', '', p.v));
      else if (p.t === 'code') parent.append(el('code', '', p.v));
      else parent.append(document.createTextNode(p.v));
    }
    return parent;
  }

  function renderBody(body) {
    var f = document.createDocumentFragment(), blocks = parseBody(body);
    for (var i = 0; i < blocks.length; i++) {
      var b = blocks[i];
      if (b.type === 'h') f.append(el('h4', '', b.text));
      else if (b.type === 'ul') {
        var ul = el('ul');
        for (var j = 0; j < b.items.length; j++) ul.append(renderInline(el('li'), b.items[j]));
        f.append(ul);
      } else f.append(renderInline(el('p'), b.inline));
    }
    return f;
  }

  function renderRelease(r, isFirst) {
    var card = el('article', 'rel glass rise in' + (isFirst ? ' first' : ''));
    var tag = String(r.tag_name || '');
    card.id = tag.replace(/[^0-9A-Za-z._-]/g, '-');
    var side = el('div', 'side');
    side.append(el('div', 'v', tag.replace(/^v/, '')));
    var when = releaseDate(r), d = el('div', 'd');
    if (when) {
      var t = el('time', '', formatTime(when));
      t.setAttribute('datetime', when);
      d.append(t);
    }
    side.append(d);
    if (isFirst) side.append(el('span', 'new', 'СВЕЖАЯ'));
    var main = el('div', 'body');
    main.append(renderBody(r.body));
    var a = el('a', 'open', 'На GitHub →');
    a.href = safeHref(r.html_url);
    a.setAttribute('rel', 'noopener');
    main.append(a);
    card.append(side, main);
    return card;
  }

  function render(view, all, query) {
    var shown = [];
    for (var i = 0; i < all.length; i++) if (matches(all[i], query)) shown.push(all[i]);
    var searching = !!normalize(query);
    var nodes = [];
    if (!shown.length) {
      var empty = el('div', 'empty glass');
      if (searching) {
        empty.append(el('p', '', 'Ничего не найдено по запросу «' + String(query).trim() + '»'));
        var btn = el('button', 'btn ghost', 'Сбросить поиск');
        btn.setAttribute('type', 'button');
        if (view.onReset) btn.addEventListener('click', view.onReset);
        empty.append(btn);
      } else empty.append(el('p', '', 'Пока нет ни одной версии.'));
      nodes.push(empty);
    } else {
      var groups = groupByDay(shown), first = !searching;
      for (var g = 0; g < groups.length; g++) {
        var sec = el('section', 'day'), h = el('h2', 'dayh', groups[g].title);
        var n = groups[g].items.length;
        h.append(el('span', 'count', ' · ' + n + ' ' + pluralRu(n, VERSIONS)));
        var box = el('div', 'news');
        for (var k = 0; k < groups[g].items.length; k++) { box.append(renderRelease(groups[g].items[k], first)); first = false; }
        sec.append(h, box);
        nodes.push(sec);
      }
    }
    view.list.replaceChildren.apply(view.list, nodes);
    if (view.count) {
      view.count.textContent = searching
        ? 'Найдено: ' + shown.length + ' из ' + all.length
        : (all.length ? all.length + ' ' + pluralRu(all.length, VERSIONS) : '');
    }
    return shown.length;
  }

  function errorNode(err) {
    var box = el('div', 'err');
    box.append(el('p', '', err.message));
    var a = el('a', '', 'Открыть релизы на GitHub');
    a.href = RELEASES;
    box.append(a);
    return box;
  }

  // ── страница
  // env — для тестов: document, location, history, fetch, storage, setTimeout, clearTimeout, clock.
  function init(env) {
    env = env || {};
    var doc = env.document || document;
    var loc = env.location || root.location;
    var hist = env.history || root.history;
    var later = env.setTimeout || setTimeout, cancel = env.clearTimeout || clearTimeout;
    var $ = function (id) { return doc.getElementById(id); };
    var clock = $('clock');
    if (clock && env.clock !== false) {
      var tick = function () { clock.textContent = formatTime(new Date()); };
      tick(); setInterval(tick, 10000);
    }
    var input = $('q'), state = $('state');
    var view = { list: $('list'), count: $('count') };
    var all = [], loaded = false, timer = 0;
    var storage = null;
    if ('storage' in env) storage = env.storage;
    else { try { storage = root.localStorage; } catch (e) { storage = null; } }

    var readHash = function () {
      var m = /^#q=(.*)$/.exec(loc.hash || '');
      if (!m) return '';
      try { return decodeURIComponent(m[1]); } catch (e) { return ''; }
    };
    var writeHash = function (q) {
      try { hist.replaceState(null, '', q ? '#q=' + encodeURIComponent(q) : loc.pathname + loc.search); } catch (e) { /* без адреса */ }
    };
    // Ссылка вида #v0.1.61: карточки появляются после загрузки, поэтому браузер сам не прокрутит.
    var revealHash = function () {
      var m = /^#([0-9A-Za-z._-]+)$/.exec(loc.hash || '');
      if (!m) return;
      var target = $(m[1]);
      if (target && target.scrollIntoView) target.scrollIntoView();
    };
    // До загрузки списка (или если он не загрузился) поиск только запоминает запрос:
    // иначе под «Загружаю…» или под ошибкой появилось бы «Пока нет ни одной версии».
    var apply = function () {
      cancel(timer);
      if (loaded) render(view, all, input.value);
      writeHash(input.value.trim());
    };
    view.onReset = function () { input.value = ''; apply(); input.focus(); };
    var setState = function (node, text) {
      if (!node && !text) { state.hidden = true; state.textContent = ''; return; }
      state.hidden = false;
      state.replaceChildren();
      if (node) state.append(node);
      if (text) state.append(el('p', '', text));
    };
    var show = function (releases) {
      all = prepare(releases);
      loaded = true;
      render(view, all, input.value);
      revealHash();
    };

    input.value = readHash();
    input.addEventListener('input', function () { cancel(timer); timer = later(apply, 150); });
    input.addEventListener('keydown', function (e) {
      if (e.key === 'Escape' && input.value) { e.preventDefault(); input.value = ''; apply(); }
    });

    var fetchFn = env.fetch || root.fetch.bind(root);
    // then(ok, fail): ошибка в отрисовке не должна выдаваться за «нет связи».
    return fetchAll(fetchFn).then(function (res) {
      // Неполный список кэш не затирает: в нём может быть больше версий.
      if (!res.partial) saveCache(storage, res.releases, Date.now());
      setState(res.partial ? errorNode(res.partial) : null);
      show(res.releases);
    }, function (err) {
      var info = err && err.kind ? err : classifyError(err);
      var cached = loadCache(storage);
      if (cached) {
        setState(errorNode(info), cacheNote(cached.savedAt));
        show(cached.releases);
      } else setState(errorNode(info));
    });
  }

  var api = {
    MONTHS_GEN: MONTHS_GEN, CACHE_KEY: CACHE_KEY,
    dayKey: dayKey, formatDayRu: formatDayRu, formatTime: formatTime, pluralRu: pluralRu,
    releaseDate: releaseDate, normalize: normalize, prepare: prepare, matches: matches,
    groupByDay: groupByDay, parseInline: parseInline, parseBody: parseBody, safeHref: safeHref,
    classifyError: classifyError, fetchAll: fetchAll,
    saveCache: saveCache, loadCache: loadCache, cacheNote: cacheNote,
    renderBody: renderBody, renderRelease: renderRelease, render: render, errorNode: errorNode,
    init: init
  };
  if (typeof module === 'object' && module.exports) module.exports = api;
  else if (typeof document !== 'undefined') init();
})(this);
