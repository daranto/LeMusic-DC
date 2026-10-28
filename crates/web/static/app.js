"use strict";

// ---------------------------------------------------------------------------
// Hilfsfunktionen
// ---------------------------------------------------------------------------

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

const esc = (s) =>
  String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

function fmt(secs) {
  if (secs == null) return "Live";
  secs = Math.max(0, Math.floor(secs));
  const h = Math.floor(secs / 3600), m = Math.floor((secs % 3600) / 60), s = secs % 60;
  const ss = String(s).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

function toast(msg, kind = "") {
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.textContent = msg;
  $("#toasts").append(el);
  setTimeout(() => el.remove(), kind === "error" ? 6000 : 3500);
}

class Unauthorized extends Error {}

async function api(method, path, body, raw = false) {
  const opts = { method, headers: { "X-LeMusic": "1" }, credentials: "same-origin" };
  if (body !== undefined) {
    if (raw) {
      opts.body = body;
      opts.headers["Content-Type"] = "text/plain";
    } else {
      opts.body = JSON.stringify(body);
      opts.headers["Content-Type"] = "application/json";
    }
  }
  const res = await fetch(`/api${path}`, opts);
  let data = null;
  try { data = await res.json(); } catch { /* leere Antwort */ }
  if (res.status === 401 && path !== "/login") {
    showLogin();
    throw new Unauthorized();
  }
  if (!res.ok) throw new Error(data?.error || `Fehler ${res.status}`);
  return data;
}

/** Führt eine Aktion aus, sperrt den Button und meldet Fehler. */
async function run(button, fn, okMsg) {
  if (button) button.disabled = true;
  try {
    const r = await fn();
    if (okMsg) toast(typeof okMsg === "function" ? okMsg(r) : okMsg, "ok");
    return r;
  } catch (e) {
    if (!(e instanceof Unauthorized)) toast(e.message, "error");
  } finally {
    if (button) button.disabled = false;
  }
}

function formData(form) {
  const out = {};
  for (const el of form.elements) {
    if (!el.name) continue;
    if (el.type === "checkbox") out[el.name] = el.checked;
    else if (el.type === "number") out[el.name] = el.value === "" ? null : Number(el.value);
    else out[el.name] = el.value;
  }
  return out;
}

function fillSelect(select, items, { empty, selected } = {}) {
  select.replaceChildren();
  if (empty !== undefined) select.append(new Option(empty, ""));
  for (const it of items) select.append(new Option(it.name, it.id));
  if (selected != null) select.value = selected;
}

// ---------------------------------------------------------------------------
// Zustand
// ---------------------------------------------------------------------------

const state = {
  guild: null,
  player: null,
  playerFetchedAt: 0,
  tab: "dashboard",
  logSeq: 0,
  playlist: null,
  timers: [],
};

// ---------------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------------

function showLogin() {
  state.timers.forEach(clearInterval);
  state.timers = [];
  $("#app-view").hidden = true;
  $("#login-view").hidden = false;
  $("#login-form input[name=username]").focus();
}

async function showApp(username) {
  $("#login-view").hidden = true;
  $("#app-view").hidden = false;
  $("#username").textContent = username;
  await refreshOverview();
  await refreshPlayer();
  state.timers.push(setInterval(refreshPlayer, 2000));
  state.timers.push(setInterval(refreshOverview, 15000));
  state.timers.push(setInterval(tickProgress, 1000));
  state.timers.push(setInterval(() => state.tab === "logs" && refreshLogs(), 2000));
}

$("#login-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const btn = $("button", ev.target);
  btn.disabled = true;
  $("#login-error").textContent = "";
  try {
    const r = await api("POST", "/login", formData(ev.target));
    ev.target.reset();
    await showApp(r.username);
  } catch (e) {
    $("#login-error").textContent = e.message;
  } finally {
    btn.disabled = false;
  }
});

$("#logout").addEventListener("click", () =>
  run(null, async () => {
    await api("POST", "/logout");
    showLogin();
  }));

// ---------------------------------------------------------------------------
// Tabs
// ---------------------------------------------------------------------------

function switchTab(tab) {
  state.tab = tab;
  $$(".tabs button").forEach((b) => b.classList.toggle("active", b.dataset.tab === tab));
  $$(".tab").forEach((s) => (s.hidden = s.id !== `tab-${tab}`));
  if (tab === "playlists") loadPlaylists();
  if (tab === "settings") loadSettings();
  if (tab === "logs") refreshLogs();
  history.replaceState(null, "", `#${tab}`);
}
$$(".tabs button").forEach((b) => b.addEventListener("click", () => switchTab(b.dataset.tab)));

// ---------------------------------------------------------------------------
// Übersicht & Wiedergabe
// ---------------------------------------------------------------------------

async function refreshOverview() {
  try {
    const o = await api("GET", "/overview");
    state.guild = o.guild;
    const chip = (label, ok) =>
      `<span class="chip"><span class="dot ${ok ? "ok" : "bad"}"></span>${esc(label)}</span>`;
    const up = o.uptime_secs;
    const uptime = up > 86400 ? `${Math.floor(up / 86400)} T ${Math.floor((up % 86400) / 3600)} h` : fmt(up);
    $("#status-row").innerHTML =
      chip(`Discord${o.guild ? ` · ${o.guild.name}` : ""}`, o.health.discord) +
      chip("Datenbank", o.health.database) +
      chip(`yt-dlp ${o.ytdlp_version}`, o.ytdlp_version !== "nicht verfügbar") +
      `<span class="chip">⏱ Laufzeit ${esc(uptime)}</span>` +
      `<span class="chip">v${esc(o.health.version)}</span>`;
    $("#ytdlp-version").textContent = o.ytdlp_version;
    const badge = $("#cookies-badge");
    badge.textContent = o.cookies ? "aktiv" : "keine";
    badge.classList.toggle("on", o.cookies);
    if (o.guild) {
      const sel = $("#voice-select");
      const prev = sel.value;
      fillSelect(sel, o.guild.voice_channels, { empty: "— aktueller Kanal des Bots —", selected: prev });
    }
  } catch (e) {
    if (!(e instanceof Unauthorized)) console.warn(e);
  }
}

async function refreshPlayer() {
  try {
    state.player = await api("GET", "/player");
    state.playerFetchedAt = Date.now();
    renderPlayer();
  } catch (e) {
    if (!(e instanceof Unauthorized)) console.warn(e);
  }
}

function channelName(id) {
  return state.guild?.voice_channels.find((c) => c.id === id)?.name;
}

function renderPlayer() {
  const p = state.player;
  if (!p) return;
  const now = p.now;
  const cover = $("#now-cover");
  if (now?.entry.track.thumbnail) {
    cover.style.backgroundImage = `url("${CSS.escape(now.entry.track.thumbnail)}")`;
    cover.textContent = "";
  } else {
    cover.style.backgroundImage = "";
    cover.textContent = "♪";
  }
  const title = $("#now-title");
  if (now) {
    const where = channelName(p.voice_channel);
    const listeners = p.listeners != null ? ` · ${p.listeners} Zuhörer` : "";
    $("#now-state").textContent =
      (now.paused ? "Pausiert" : now.ready ? "Läuft" : "Lädt …") + (where ? ` · 🔊 ${where}` : "") + listeners;
    title.textContent = now.entry.track.title;
    title.href = now.entry.track.url;
    $("#now-author").textContent = now.entry.track.author;
    $("#btn-playpause").textContent = now.paused ? "▶" : "⏸";
  } else {
    $("#now-state").textContent = p.voice_channel ? "Bereit" : "Nichts läuft";
    title.textContent = "Nichts läuft gerade";
    title.removeAttribute("href");
    $("#now-author").textContent = "Füge rechts etwas hinzu.";
    $("#btn-playpause").textContent = "⏯";
  }
  tickProgress();

  const labels = { off: "🔁 aus", one: "🔂 Track", all: "🔁 alle" };
  $("#btn-repeat").textContent = labels[p.repeat] || "🔁";
  const vol = $("#volume");
  if (document.activeElement !== vol) vol.value = p.volume;
  $("#volume-label").textContent = `${p.volume} %`;

  $("#queue-meta").textContent = p.queue.length ? `· ${p.queue.length} Tracks · ${fmt(p.queue_duration)}` : "";
  const list = $("#queue");
  if (!p.queue.length) {
    list.innerHTML = `<li class="empty">Die Warteschlange ist leer.</li>`;
    return;
  }
  list.innerHTML = p.queue
    .map((e, i) => `
      <li>
        <span class="pos">${i + 1}</span>
        ${e.track.thumbnail ? `<img loading="lazy" alt="" src="${esc(e.track.thumbnail)}">` : ""}
        <div class="meta">
          <a href="${esc(e.track.url)}" target="_blank" rel="noopener">${esc(e.track.title)}</a>
          <small>${esc(e.track.author)}${e.requester ? "" : " · Web/Autoplaylist"}</small>
        </div>
        <span class="dur">${fmt(e.track.duration)}</span>
        <div class="item-actions">
          <button class="btn" data-act="skipto" data-i="${i}" title="Jetzt spielen">▶</button>
          <button class="btn" data-act="up" data-i="${i}" title="Nach oben" ${i === 0 ? "disabled" : ""}>↑</button>
          <button class="btn" data-act="down" data-i="${i}" title="Nach unten" ${i === p.queue.length - 1 ? "disabled" : ""}>↓</button>
          <button class="btn danger" data-act="remove" data-i="${i}" title="Entfernen">✕</button>
        </div>
      </li>`)
    .join("");
}

/** Fortschritt zwischen zwei Abfragen lokal weiterzählen. */
function tickProgress() {
  const now = state.player?.now;
  if (!now) {
    $("#now-progress").style.width = "0%";
    $("#now-pos").textContent = "0:00";
    $("#now-len").textContent = "0:00";
    return;
  }
  let pos = now.position;
  if (!now.paused && now.ready) pos += (Date.now() - state.playerFetchedAt) / 1000;
  const len = now.entry.track.duration;
  if (len) pos = Math.min(pos, len);
  $("#now-pos").textContent = fmt(pos);
  $("#now-len").textContent = len ? fmt(len) : "Live";
  $("#now-progress").style.width = len ? `${(pos / len) * 100}%` : "100%";
}

async function action(name, body, button, okMsg) {
  await run(button, () => api("POST", `/player/${name}`, body ?? {}), okMsg);
  refreshPlayer();
}

$("#btn-playpause").addEventListener("click", (ev) =>
  action(state.player?.now?.paused ? "resume" : "pause", null, ev.currentTarget));
$("#btn-skip").addEventListener("click", (ev) => action("skip", null, ev.currentTarget, "Übersprungen"));
$("#btn-stop").addEventListener("click", (ev) => {
  if (confirm("Wiedergabe stoppen, Warteschlange leeren und Kanal verlassen?"))
    action("stop", null, ev.currentTarget, "Gestoppt");
});
$("#btn-shuffle").addEventListener("click", (ev) => action("shuffle", null, ev.currentTarget, "Gemischt"));
$("#btn-repeat").addEventListener("click", (ev) => {
  const next = { off: "one", one: "all", all: "off" }[state.player?.repeat || "off"];
  action("repeat", { mode: next }, ev.currentTarget);
});
$("#btn-clear").addEventListener("click", (ev) => {
  if (confirm("Warteschlange leeren? Der aktuelle Track läuft weiter."))
    action("clear", null, ev.currentTarget, "Warteschlange geleert");
});
$("#btn-save-queue").addEventListener("click", (ev) => {
  const name = prompt("Name der Playlist:");
  if (name) run(ev.currentTarget, () => api("POST", "/playlists/save-queue", { name }), (r) => `${r.saved} Tracks gespeichert`);
});
$("#volume").addEventListener("change", (ev) => action("volume", { percent: Number(ev.target.value) }));
$("#volume").addEventListener("input", (ev) => ($("#volume-label").textContent = `${ev.target.value} %`));
$("#seek-form").addEventListener("submit", (ev) => {
  ev.preventDefault();
  action("seek", formData(ev.target), $("button", ev.target), "Gespult");
  ev.target.reset();
});

$("#queue").addEventListener("click", (ev) => {
  const btn = ev.target.closest("button[data-act]");
  if (!btn) return;
  const i = Number(btn.dataset.i);
  const act = btn.dataset.act;
  if (act === "skipto") action("skipto", { index: i }, btn);
  if (act === "remove") action("remove", { index: i }, btn);
  if (act === "up") action("move", { from: i, to: i - 1 }, btn);
  if (act === "down") action("move", { from: i, to: i + 1 }, btn);
});

$("#play-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const body = formData(ev.target);
  if (!body.voice_channel) delete body.voice_channel;
  const r = await run($("button[type=submit]", ev.target), () => api("POST", "/player/play", body), (r) =>
    r.added === 1 ? (r.position === 0 ? "Spielt jetzt" : `Eingereiht an Position ${r.position}`) : `${r.added} Tracks eingereiht`);
  if (r) {
    $("input[name=query]", ev.target).value = "";
    if (r.skipped_too_long) toast(`${r.skipped_too_long} Track(s) zu lang – ausgelassen`);
  }
  refreshPlayer();
});

// ---------------------------------------------------------------------------
// Playlists
// ---------------------------------------------------------------------------

async function loadPlaylists() {
  const lists = await run(null, () => api("GET", "/playlists"));
  if (!lists) return;
  const ul = $("#playlist-list");
  ul.innerHTML = lists.length
    ? lists.map((p) => `
        <li data-name="${esc(p.name)}" class="${p.name === state.playlist ? "active" : ""}">
          <div class="meta"><a>${esc(p.name)}</a><small>${p.track_count} Tracks</small></div>
        </li>`).join("")
    : `<li class="empty">Noch keine Playlists.</li>`;
  if (state.playlist && !lists.some((p) => p.name === state.playlist)) {
    state.playlist = null;
    $("#playlist-detail").innerHTML = `<p class="muted">Playlist auswählen.</p>`;
  }
}

$("#playlist-list").addEventListener("click", (ev) => {
  const li = ev.target.closest("li[data-name]");
  if (li) showPlaylist(li.dataset.name);
});

async function showPlaylist(name) {
  state.playlist = name;
  $$("#playlist-list li").forEach((li) => li.classList.toggle("active", li.dataset.name === name));
  const p = await run(null, () => api("GET", `/playlists/${encodeURIComponent(name)}`));
  if (!p) return;
  const voice = state.guild?.voice_channels ?? [];
  $("#playlist-detail").innerHTML = `
    <div class="card-head">
      <h2>${esc(p.name)} <span class="muted">· ${p.tracks.length} Tracks</span></h2>
      <button class="btn small danger" id="pl-delete">Löschen</button>
    </div>
    <form class="row" id="pl-play">
      <select name="voice_channel"><option value="">— aktueller Kanal des Bots —</option>
        ${voice.map((c) => `<option value="${esc(c.id)}">${esc(c.name)}</option>`).join("")}</select>
      <label class="check"><input type="checkbox" name="shuffle"> Zufällig</label>
      <button class="btn primary small" type="submit">▶ Abspielen</button>
    </form>
    <form class="row" id="pl-add">
      <input name="query" placeholder="Link oder Suche hinzufügen" required>
      <label class="check"><input type="checkbox" name="soundcloud"> SoundCloud</label>
      <button class="btn small" type="submit">Hinzufügen</button>
    </form>
    <ol class="queue">
      ${p.tracks.map((t, i) => `
        <li>
          <span class="pos">${i + 1}</span>
          ${t.thumbnail ? `<img loading="lazy" alt="" src="${esc(t.thumbnail)}">` : ""}
          <div class="meta"><a href="${esc(t.url)}" target="_blank" rel="noopener">${esc(t.title)}</a><small>${esc(t.author)}</small></div>
          <span class="dur">${fmt(t.duration)}</span>
          <div class="item-actions"><button class="btn danger" data-i="${i}" title="Entfernen">✕</button></div>
        </li>`).join("") || `<li class="empty">Leer.</li>`}
    </ol>`;
  const path = `/playlists/${encodeURIComponent(name)}`;
  $("#pl-delete").addEventListener("click", (ev) => {
    if (!confirm(`Playlist „${name}“ löschen?`)) return;
    run(ev.currentTarget, () => api("DELETE", path), "Gelöscht").then(() => {
      state.playlist = null;
      $("#playlist-detail").innerHTML = `<p class="muted">Playlist auswählen.</p>`;
      loadPlaylists();
    });
  });
  $("#pl-play").addEventListener("submit", (ev) => {
    ev.preventDefault();
    const body = formData(ev.target);
    if (!body.voice_channel) delete body.voice_channel;
    run($("button", ev.target), () => api("POST", `${path}/play`, body), (r) => `${r.added} Tracks eingereiht`)
      .then(refreshPlayer);
  });
  $("#pl-add").addEventListener("submit", (ev) => {
    ev.preventDefault();
    run($("button", ev.target), () => api("POST", `${path}/add`, formData(ev.target)), (r) => `${r.added} hinzugefügt`)
      .then((r) => r && (showPlaylist(name), loadPlaylists()));
  });
  $$("#playlist-detail .queue button[data-i]").forEach((b) =>
    b.addEventListener("click", () =>
      run(b, () => api("POST", `${path}/remove`, { index: Number(b.dataset.i) })).then(() => {
        showPlaylist(name);
        loadPlaylists();
      })));
}

// ---------------------------------------------------------------------------
// Einstellungen
// ---------------------------------------------------------------------------

async function loadSettings() {
  const [bot, guild, lists] = await Promise.all([
    run(null, () => api("GET", "/settings/bot")),
    run(null, () => api("GET", "/settings/guild")),
    run(null, () => api("GET", "/playlists")),
  ]);
  if (bot) {
    const f = $("#bot-settings");
    f.status_text.value = bot.status_text;
    f.default_volume.value = bot.default_volume;
    f.idle_timeout_secs.value = bot.idle_timeout_secs;
    f.pause_when_alone.checked = bot.pause_when_alone;
  }
  if (guild) {
    const f = $("#guild-settings");
    const g = state.guild;
    fillSelect(f.dj_role, g?.roles ?? [], { empty: "— keine (alle sind DJ) —", selected: guild.dj_role ?? "" });
    fillSelect(f.text_channel, g?.text_channels ?? [], { empty: "— alle Kanäle —", selected: guild.text_channel ?? "" });
    fillSelect(f.announce_channel, g?.text_channels ?? [], {
      empty: "— Kanal des letzten Befehls —",
      selected: guild.announce_channel ?? "",
    });
    fillSelect(f.voice_channel, g?.voice_channels ?? [], { empty: "— alle Kanäle —", selected: guild.voice_channel ?? "" });
    fillSelect(f.autoplaylist, (lists ?? []).map((p) => ({ id: p.name, name: p.name })), {
      empty: "— keine —",
      selected: guild.autoplaylist ?? "",
    });
    f.skip_ratio.value = guild.skip_ratio;
    f.max_track_mins.value = Math.round(guild.max_track_secs / 60);
    f.announce_tracks.checked = guild.announce_tracks;
  }
}

$("#bot-settings").addEventListener("submit", (ev) => {
  ev.preventDefault();
  run($("button", ev.target), () => api("PUT", "/settings/bot", formData(ev.target)), "Gespeichert");
});

$("#guild-settings").addEventListener("submit", (ev) => {
  ev.preventDefault();
  const d = formData(ev.target);
  const body = {
    dj_role: d.dj_role || null,
    text_channel: d.text_channel || null,
    announce_channel: d.announce_channel || null,
    voice_channel: d.voice_channel || null,
    skip_ratio: d.skip_ratio ?? 55,
    max_track_secs: (d.max_track_mins ?? 0) * 60,
    autoplaylist: d.autoplaylist || null,
    announce_tracks: d.announce_tracks,
  };
  run($("button", ev.target), () => api("PUT", "/settings/guild", body), "Gespeichert");
});

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

$("#btn-ytdlp-update").addEventListener("click", (ev) => {
  const btn = ev.currentTarget;
  btn.textContent = "Aktualisiere …";
  run(btn, () => api("POST", "/system/ytdlp-update"), (r) => `yt-dlp ${r.version}`).finally(() => {
    btn.textContent = "Jetzt aktualisieren";
    refreshOverview();
  });
});

$("#selftest-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const out = $("#selftest-result");
  out.hidden = false;
  out.textContent = "Teste … (ca. 15–25 Sekunden)";
  const r = await run($("button", ev.target), () => api("POST", "/system/selftest", formData(ev.target)));
  if (!r) {
    out.textContent = "Fehlgeschlagen – Details in der Meldung unten rechts.";
    return;
  }
  const t = r.report;
  out.textContent =
    `${r.ok ? "✅ OK" : "❌ Aussetzer erkannt"}\n` +
    `Track:        ${t.title} – ${t.author}\n` +
    `Auflösen:     ${t.resolve_ms} ms\n` +
    `Stream offen: ${t.open_ms} ms\n` +
    `Gelesen:      ${t.decoded_secs.toFixed(1)} s, langsamster Lesevorgang ${t.slowest_read_ms.toFixed(1)} ms, Blockaden: ${t.stalls}`;
});

$("#btn-cookies-upload").addEventListener("click", async (ev) => {
  const file = $("#cookies-file").files[0];
  if (!file) return toast("Bitte zuerst eine cookies.txt auswählen", "error");
  const text = await file.text();
  run(ev.currentTarget, () => api("PUT", "/system/cookies", text, true), "Cookies gespeichert").then(refreshOverview);
});
$("#btn-cookies-delete").addEventListener("click", (ev) =>
  run(ev.currentTarget, () => api("DELETE", "/system/cookies"), "Cookies entfernt").then(refreshOverview));

// ---------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------

const LEVELS = ["TRACE", "DEBUG", "INFO", "WARN", "ERROR"];
let logLines = [];

async function refreshLogs() {
  try {
    const lines = await api("GET", `/logs?after=${state.logSeq}`);
    if (lines.length) {
      state.logSeq = lines[lines.length - 1].seq;
      logLines = logLines.concat(lines).slice(-1000);
    }
    renderLogs();
  } catch (e) {
    if (!(e instanceof Unauthorized)) console.warn(e);
  }
}

function renderLogs() {
  const min = LEVELS.indexOf($("#log-level").value);
  const box = $("#logs");
  const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 40;
  box.innerHTML = logLines
    .filter((l) => LEVELS.indexOf(l.level) >= min)
    .map((l) => {
      const t = new Date(l.ts).toLocaleTimeString("de-DE");
      return `<div class="${esc(l.level)}"><span class="t">${t}</span> ${esc(l.level.padEnd(5))} ${esc(l.message)}</div>`;
    })
    .join("") || `<div class="t">Keine Einträge.</div>`;
  if (atBottom) box.scrollTop = box.scrollHeight;
}
$("#log-level").addEventListener("change", renderLogs);

// ---------------------------------------------------------------------------
// Konto
// ---------------------------------------------------------------------------

$("#password-form").addEventListener("submit", (ev) => {
  ev.preventDefault();
  run($("button", ev.target), () => api("POST", "/account/password", formData(ev.target)), "Passwort geändert").then(
    (r) => r && ev.target.reset());
});

// ---------------------------------------------------------------------------
// Start
// ---------------------------------------------------------------------------

(async () => {
  try {
    const me = await api("GET", "/me");
    await showApp(me.username);
    const tab = location.hash.slice(1);
    if (tab && $(`#tab-${CSS.escape(tab)}`)) switchTab(tab);
  } catch (e) {
    if (!(e instanceof Unauthorized)) showLogin();
  }
})();
