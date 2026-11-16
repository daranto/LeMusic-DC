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

/** Hinweis unten rechts; optional mit Knopf (z. B. „Rückgängig“). */
function toast(msg, kind = "", act) {
  const el = document.createElement("div");
  el.className = `toast ${kind}`;
  el.append(msg);
  if (act) {
    const b = document.createElement("button");
    b.className = "btn small";
    b.textContent = act.label;
    b.addEventListener("click", () => {
      el.remove();
      act.fn();
    });
    el.append(b);
  }
  $("#toasts").append(el);
  setTimeout(() => el.remove(), act ? 10000 : kind === "error" ? 6000 : 3500);
}

/** Link in die Zwischenablage (ohne HTTPS nicht überall erlaubt). */
async function copyLink(url) {
  try {
    await navigator.clipboard.writeText(url);
    toast("Link kopiert", "ok");
  } catch {
    prompt("Link kopieren:", url);
  }
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
  if (!res.ok) {
    const err = new Error(data?.error || `Fehler ${res.status}`);
    err.status = res.status;
    throw err;
  }
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
  playlists: [],
  autoplaylist: null,
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
  loadPlaylists();
  api("GET", "/settings/guild").then((g) => (state.autoplaylist = g.autoplaylist)).catch(() => {});
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
    state.timeZone = o.time_zone;
    renderYtdlpLast(o.ytdlp_last_update);
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

  renderQueue();
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
$("#volume").addEventListener("change", (ev) => action("volume", { percent: Number(ev.target.value) }));
$("#volume").addEventListener("input", (ev) => ($("#volume-label").textContent = `${ev.target.value} %`));
$("#seek-form").addEventListener("submit", (ev) => {
  ev.preventDefault();
  action("seek", formData(ev.target), $("button", ev.target), "Gespult");
  ev.target.reset();
});

// Warteschlange -------------------------------------------------------------

async function queueEdit(body, { okMsg, undo = false } = {}) {
  try {
    const r = await api("POST", "/player/queue", body);
    const msg = typeof okMsg === "function" ? okMsg(r) : okMsg;
    if (undo && r.affected) {
      toast(msg || "Geändert", "ok", {
        label: "Rückgängig",
        fn: () => queueEdit({ op: "restore" }, { okMsg: "Wiederhergestellt" }),
      });
    } else if (msg) {
      toast(msg, "ok");
    }
    return r;
  } catch (e) {
    if (!(e instanceof Unauthorized)) toast(e.message, "error");
  } finally {
    refreshPlayer();
  }
}

const queueEntry = (id) => state.player?.queue.find((e) => e.id === id);
const queueTracks = (ids) => ids.map(queueEntry).filter(Boolean).map((e) => e.track);

/** Lokale Vorschau einer Verschiebung über IDs. */
function queueMovedLocally(ids, before, front) {
  const q = state.player.queue;
  const set = new Set(ids);
  const moved = q.filter((e) => set.has(e.id));
  const rest = q.filter((e) => !set.has(e.id));
  let at = front ? 0 : before == null ? rest.length : rest.findIndex((e) => e.id === before);
  if (at < 0) at = rest.length;
  rest.splice(at, 0, ...moved);
  state.player = { ...state.player, queue: rest };
  renderQueue();
}

function queueMove(ids, { before = null, front = false } = {}) {
  queueMovedLocally(ids, before, front);
  queueList.clearSelection();
  return queueEdit({ op: "move", ids, before, front });
}

function queueRemove(ids) {
  const first = queueEntry(ids[0]);
  queueList.clearSelection();
  return queueEdit({ op: "remove", ids }, {
    undo: true,
    okMsg: ids.length === 1 && first ? `„${first.track.title}“ entfernt` : `${ids.length} Titel entfernt`,
  });
}

async function queuePlayNow(id) {
  const r = await queueEdit({ op: "move", ids: [id], front: true });
  if (r) action("skip", null, null, "Spielt jetzt");
}

async function queueToPlaylist(name, ids) {
  const tracks = queueTracks(ids);
  if (!tracks.length) return;
  const r = await run(null, () => api("POST", `${plPath(name)}/append`, { tracks }),
    (r) => `${tracks.length} Titel zu „${name}“ hinzugefügt (jetzt ${r.total})`);
  if (r) {
    queueList.clearSelection();
    loadPlaylists();
  }
}

function newPlaylistFor(fn) {
  const n = prompt("Name der neuen Playlist:");
  if (n && n.trim()) fn(n.trim());
}

function queuePlaylistMenu(ids) {
  return [
    { heading: "In Playlist kopieren" },
    ...state.playlists.map((n) => ({ label: n, sub: true, fn: () => queueToPlaylist(n, ids) })),
    { label: "＋ Neue Playlist …", sub: true, fn: () => newPlaylistFor((n) => queueToPlaylist(n, ids)) },
  ];
}

const queueList = TrackList.create($("#queue"), {
  empty: "Die Warteschlange ist leer.",
  quick: [
    { act: "now", label: "▶", title: "Jetzt spielen" },
    { act: "remove", label: "✕", title: "Entfernen", danger: true },
  ],
  bulk: [
    { act: "next", label: "⏭ Als Nächstes" },
    { act: "end", label: "⤓ Ans Ende" },
    { act: "playlist", label: "In Playlist …" },
    { act: "remove", label: "✕ Entfernen", danger: true },
  ],
  onAction: (act, id) => {
    if (act === "now") queuePlayNow(id);
    if (act === "remove") queueRemove([id]);
  },
  onBulk: (act, ids, btn) => {
    if (act === "next") queueMove(ids, { front: true });
    if (act === "end") queueMove(ids);
    if (act === "playlist") TrackList.menuAt(btn, queuePlaylistMenu(ids));
    if (act === "remove") queueRemove(ids);
  },
  menu: (id) => {
    const q = state.player.queue;
    const i = q.findIndex((e) => e.id === id);
    const e = q[i];
    return [
      { label: "▶ Jetzt spielen", fn: () => queuePlayNow(id) },
      { label: "⏭ Als Nächstes", disabled: i === 0, fn: () => queueMove([id], { front: true }) },
      { label: "⤓ Ans Ende", disabled: i === q.length - 1, fn: () => queueMove([id]) },
      { label: "⏩ Bis hierhin überspringen", disabled: i === 0, fn: () => action("skipto", { index: i }, null, "Gesprungen") },
      "-",
      ...queuePlaylistMenu([id]),
      "-",
      { label: "🔗 Link kopieren", fn: () => copyLink(e.track.url) },
      { label: "✕ Entfernen", danger: true, fn: () => queueRemove([id]) },
    ];
  },
  onReorder: (ids, target, pos) => {
    let before = target;
    if (pos === "after") {
      const q = state.player.queue;
      const set = new Set(ids);
      const i = q.findIndex((e) => e.id === target);
      before = q.slice(i + 1).find((e) => !set.has(e.id))?.id ?? null;
    }
    queueMove(ids, { before });
    return ids[0];
  },
  dockTargets: () => [
    ...state.playlists.map((n) => ({ id: `pl:${n}`, label: `＋ ${n}` })),
    { id: "newpl", label: "＋ Neue Playlist" },
  ],
  onDropTarget: (id, ids) => {
    if (id.startsWith("pl:")) queueToPlaylist(id.slice(3), ids);
    if (id === "newpl") newPlaylistFor((n) => queueToPlaylist(n, ids));
  },
});

function renderQueue() {
  const p = state.player;
  $("#queue-meta").textContent = p.queue.length ? `· ${p.queue.length} Titel · ${fmt(p.queue_duration)}` : "";
  queueList.setItems(p.queue.map((e) => ({
    key: e.id,
    track: e.track,
    cls: e.autoplay ? "auto" : "",
    sub: e.autoplay ? ` · <span class="tag">Autoplaylist</span>` : e.requester ? "" : " · Web",
  })));
}

$("#btn-queue-more").addEventListener("click", (ev) => {
  const q = state.player?.queue ?? [];
  const autos = q.filter((e) => e.autoplay).length;
  TrackList.menuAt(ev.currentTarget, [
    { label: "🔀 Mischen", disabled: q.length < 2, fn: () => action("shuffle", null, null, "Gemischt") },
    { label: "♊ Duplikate entfernen", disabled: q.length < 2, fn: () => queueEdit({ op: "dedupe" }, {
      undo: true,
      okMsg: (r) => r.affected ? `${r.affected} doppelte Titel entfernt` : "Keine Duplikate gefunden",
    }) },
    { label: `🔁 Autoplaylist-Titel entfernen${autos ? ` (${autos})` : ""}`, disabled: !autos, fn: () => queueEdit({ op: "remove_autoplay" }, {
      undo: true, okMsg: (r) => `${r.affected} Autoplaylist-Titel entfernt`,
    }) },
    "-",
    { label: "💾 Als Playlist speichern …", disabled: !q.length && !state.player?.now, fn: saveQueue },
    { label: "🗑 Leeren", danger: true, disabled: !q.length, fn: clearQueue },
  ]);
});

function saveQueue() {
  const name = prompt("Name der Playlist (aktueller Titel + Warteschlange):");
  if (name) run(null, () => api("POST", "/playlists/save-queue", { name }), (r) => `${r.saved} Titel gespeichert`).then(loadPlaylists);
}

async function clearQueue() {
  const r = await run(null, () => api("POST", "/player/clear"));
  refreshPlayer();
  if (r) toast("Warteschlange geleert – der aktuelle Titel läuft weiter", "ok", {
    label: "Rückgängig",
    fn: () => queueEdit({ op: "restore" }, { okMsg: "Wiederhergestellt" }),
  });
}

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

$("#btn-join").addEventListener("click", (ev) => {
  const voice_channel = $("#voice-select").value;
  if (!voice_channel) return toast("Bitte einen Sprachkanal auswählen", "error");
  run(ev.currentTarget, () => api("POST", "/player/join", { voice_channel }), (r) =>
    r.autoplaylist ? `Beigetreten – spiele Autoplaylist „${r.autoplaylist}“` : "Beigetreten").then(refreshPlayer);
});

// Drag & Drop: Links auf „Hinzufügen“ oder die Warteschlange ziehen.
const noLinkToast = () => toast("Das war kein YouTube- oder SoundCloud-Link", "error");

async function enqueueLinks(links) {
  const voice_channel = $("#voice-select").value || undefined;
  let added = 0;
  for (const query of links) {
    const r = await run(null, () => api("POST", "/player/play", { query, voice_channel }));
    if (r) added += r.added;
  }
  if (added) toast(added === 1 ? "1 Titel eingereiht" : `${added} Titel eingereiht`, "ok");
  refreshPlayer();
}
LeMusicDnD.attach($("#play-form"), enqueueLinks, noLinkToast);
LeMusicDnD.attach($("#queue-card"), enqueueLinks, noLinkToast);

// ---------------------------------------------------------------------------
// Playlists
// ---------------------------------------------------------------------------

/** Aktuell geöffnete Playlist (vom Server, mit Revision). */
const pl = {
  data: null,
  check: null,
  checkTimer: 0,
};

const plPath = (name) => `/playlists/${encodeURIComponent(name)}`;

async function addLinksToPlaylist(name, links) {
  let added = 0;
  for (const query of links) {
    const r = await run(null, () => api("POST", `${plPath(name)}/add`, { query }));
    if (r) added += r.added;
  }
  if (added) toast(`${added} Titel zu „${name}“ hinzugefügt`, "ok");
  await loadPlaylists();
  if (pl.data && sameName(pl.data.name, name)) showPlaylist(pl.data.name);
}

LeMusicDnD.attach($("#playlist-detail"), (links) => {
  if (!pl.data) return toast("Bitte zuerst eine Playlist auswählen – oder auf „Neue Playlist“ ziehen", "error");
  addLinksToPlaylist(pl.data.name, links);
}, noLinkToast);

LeMusicDnD.attach($("#playlist-create"), async (links) => {
  const form = $("#playlist-create");
  const name = form.elements.name.value.trim();
  if (!name) {
    form.elements.query.value = links.join(" ");
    form.elements.name.focus();
    return toast(links.length > 1 ? "Namen eingeben – mehrere Links: bitte auf eine bestehende Playlist ziehen" : "Jetzt noch einen Namen eingeben", "");
  }
  await addLinksToPlaylist(name, links);
  form.reset();
  showPlaylist(name);
}, noLinkToast);

async function loadPlaylists() {
  const lists = await run(null, () => api("GET", "/playlists"));
  if (!lists) return;
  state.playlists = lists.map((p) => p.name);
  const ul = $("#playlist-list");
  const current = pl.data?.name;
  ul.innerHTML = lists.length
    ? lists.map((p) => `
        <li data-name="${esc(p.name)}" data-drop-target="pl:${esc(p.name)}" class="${current && sameName(p.name, current) ? "active" : ""}">
          <div class="meta"><strong>${esc(p.name)}</strong><small>${p.track_count} Titel · ${fmt(p.duration_secs)}${state.autoplaylist && sameName(state.autoplaylist, p.name) ? " · 🔁 Autoplaylist" : ""}</small></div>
        </li>`).join("")
    : `<li class="empty">Noch keine Playlists.</li>`;
  // Links direkt auf eine Playlist in der Liste ziehen.
  $$("#playlist-list li[data-name]").forEach((li) =>
    LeMusicDnD.attach(li, (links) => addLinksToPlaylist(li.dataset.name, links), noLinkToast));
  if (current && !lists.some((p) => sameName(p.name, current))) closePlaylist();
}

function closePlaylist() {
  pl.data = null;
  stopCheckPolling();
  $("#playlist-detail").innerHTML = `<p class="muted">Playlist auswählen.</p>`;
}

$("#playlist-create").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const { name, query, soundcloud } = formData(ev.target);
  const btn = $("button[type=submit]", ev.target);
  const r = await run(btn, () => api("POST", `${plPath(name.trim())}/add`, { query, soundcloud }),
    (r) => `${r.added} Titel hinzugefügt – Playlist hat jetzt ${r.total}`);
  if (r) {
    ev.target.reset();
    await loadPlaylists();
    showPlaylist(name.trim());
  }
});

$("#playlist-import").addEventListener("change", async (ev) => {
  const file = ev.target.files[0];
  ev.target.value = "";
  if (!file) return;
  let data;
  try {
    data = JSON.parse(await file.text());
  } catch {
    return toast("Die Datei ist keine gültige JSON-Datei", "error");
  }
  if (!Array.isArray(data?.tracks)) return toast("In der Datei wurden keine Titel gefunden", "error");
  const name = prompt("Name der importierten Playlist:", data.name || file.name.replace(/\.json$/i, ""));
  if (!name) return;
  const r = await run(null, () => api("POST", "/playlists/import", { name, source: data.source || null, tracks: data.tracks }),
    (r) => `„${r.name}“ mit ${r.total} Titeln importiert`);
  if (r) {
    await loadPlaylists();
    showPlaylist(r.name);
  }
});

$("#playlist-list").addEventListener("click", (ev) => {
  const li = ev.target.closest("li[data-name]");
  if (li) showPlaylist(li.dataset.name);
});

const sameName = (a, b) => String(a).toLowerCase() === String(b).toLowerCase();

const plList = { list: null };

async function showPlaylist(name) {
  const p = await run(null, () => api("GET", plPath(name)));
  if (!p) return;
  if (!pl.data || !sameName(pl.data.name, p.name)) {
    stopCheckPolling();
    pl.check = null;
    renderPlaylistShell(p);
    pollCheck(p.name, true);
  }
  setPlaylist(p);
}

/** Übernimmt einen neuen Stand vom Server. */
function setPlaylist(p, { keepSelection = false } = {}) {
  pl.data = p;
  $$("#playlist-list li").forEach((li) => li.classList.toggle("active", !!li.dataset.name && sameName(li.dataset.name, p.name)));
  $("#pl-title").textContent = p.name;
  $("#pl-meta").textContent = `· ${p.tracks.length} Titel · ${fmt(p.duration)}`;
  const src = $("#pl-source");
  src.hidden = !p.source;
  if (p.source) {
    const a = $("#pl-source-link");
    a.href = p.source;
    a.textContent = p.source.replace(/^https?:\/\/(www\.)?/, "");
  }
  if (!keepSelection) plList.list.clearSelection();
  renderPlaylistItems();
  renderCheck();
}

function renderPlaylistItems() {
  const gone = new Map((pl.check?.gone || []).map((g) => [g.url, g.reason]));
  plList.list.setItems(pl.data.tracks.map((t, i) => ({
    key: i,
    track: t,
    cls: gone.has(t.url) ? "gone" : "",
    sub: gone.has(t.url) ? ` · <span class="tag bad">nicht verfügbar</span>` : "",
    title: gone.has(t.url) ? `Nicht mehr verfügbar: ${gone.get(t.url)}` : "",
  })));
}

function renderPlaylistShell(p) {
  const voice = state.guild?.voice_channels ?? [];
  const detail = $("#playlist-detail");
  detail.innerHTML = `
    <div class="card-head">
      <h2><span id="pl-title"></span> <span class="muted" id="pl-meta"></span></h2>
      <div class="row">
        <button class="btn small" id="pl-more" aria-haspopup="menu">⋯ Mehr</button>
      </div>
    </div>
    <p class="drop-note" id="pl-source" hidden>Quelle: <a id="pl-source-link" target="_blank" rel="noopener"></a>
      <button class="btn small" id="pl-sync" title="Neue Titel aus der Quelle übernehmen">↻ Aktualisieren</button></p>
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
    <div class="check-box" id="pl-check" hidden></div>
    <div id="pl-tracks"></div>`;

  plList.list = TrackList.create($("#pl-tracks"), {
    empty: "Leer – Links hierher ziehen oder oben hinzufügen.",
    dockHint: "Hierher ziehen (mit Shift: verschieben statt kopieren):",
    quick: [
      { act: "next", label: "⏭", title: "Als Nächstes spielen" },
      { act: "remove", label: "✕", title: "Entfernen", danger: true },
    ],
    bulk: [
      { act: "next", label: "⏭ Als Nächstes" },
      { act: "queue", label: "➕ Warteschlange" },
      { act: "top", label: "⤒ Nach oben" },
      { act: "bottom", label: "⤓ Nach unten" },
      { act: "copy", label: "Kopieren nach …" },
      { act: "move", label: "Verschieben nach …" },
      { act: "remove", label: "✕ Entfernen", danger: true },
    ],
    onAction: (act, i, btn) => plTrackAction(act, [i], btn),
    onBulk: (act, keys, btn) => plTrackAction(act, keys, btn),
    menu: (i) => plTrackMenu(i),
    onReorder: (keys, target, pos) => {
      const to = pos === "before" ? target : target + 1;
      plEdit([{ op: "move", indices: keys, to }], { quiet: true, optimistic: () => movedLocally(pl.data.tracks, keys, to) });
      // Fokus folgt dem verschobenen Titel.
      const before = keys.filter((k) => k < to).length;
      return to - before;
    },
    dockTargets: () => [
      { id: "queue:next", label: "⏭ Als Nächstes" },
      { id: "queue:end", label: "➕ Warteschlange" },
      ...state.playlists.filter((n) => !sameName(n, pl.data.name)).map((n) => ({ id: `pl:${n}`, label: `＋ ${n}` })),
    ],
    onDropTarget: (id, keys, { shift }) => {
      if (id === "queue:next") return plTrackAction("next", keys);
      if (id === "queue:end") return plTrackAction("queue", keys);
      if (id.startsWith("pl:")) {
        const target = id.slice(3);
        if (sameName(target, pl.data.name)) return;
        return copyToPlaylist(target, keys, shift);
      }
    },
  });

  $("#pl-more").addEventListener("click", (ev) => TrackList.menuAt(ev.currentTarget, playlistMenu()));
  $("#pl-sync").addEventListener("click", (ev) => syncPlaylist(ev.currentTarget));
  $("#pl-play").addEventListener("submit", (ev) => {
    ev.preventDefault();
    const body = formData(ev.target);
    if (!body.voice_channel) delete body.voice_channel;
    run($("button", ev.target), () => api("POST", `${plPath(pl.data.name)}/play`, body), (r) => `${r.added} Titel eingereiht`)
      .then(refreshPlayer);
  });
  $("#pl-add").addEventListener("submit", (ev) => {
    ev.preventDefault();
    const name = pl.data.name;
    run($("button", ev.target), () => api("POST", `${plPath(name)}/add`, formData(ev.target)), (r) => `${r.added} hinzugefügt`)
      .then((r) => {
        if (!r) return;
        ev.target.elements.query.value = "";
        showPlaylist(name);
        loadPlaylists();
      });
  });
}

/** Lokale Vorschau einer Verschiebung (sofortiges Feedback). */
function movedLocally(tracks, indices, to) {
  const set = new Set(indices);
  const moved = indices.slice().sort((a, b) => a - b).map((i) => tracks[i]);
  const rest = tracks.filter((_, i) => !set.has(i));
  const at = to - indices.filter((i) => i < to).length;
  rest.splice(at, 0, ...moved);
  return rest;
}

/**
 * Bearbeitung mit Revision. Bei Konflikt (409) wird neu geladen. Mit
 * `undo` erscheint ein „Rückgängig“-Knopf, der den alten Stand herstellt.
 */
async function plEdit(ops, { okMsg, undo = false, quiet = false, optimistic } = {}) {
  const before = pl.data;
  if (optimistic) {
    pl.data = { ...before, tracks: optimistic() };
    plList.list.clearSelection();
    renderPlaylistItems();
  }
  try {
    const r = await api("POST", `${plPath(before.name)}/edit`, { revision: before.revision, ops });
    setPlaylist(r);
    loadPlaylists();
    const msg = typeof okMsg === "function" ? okMsg(r) : okMsg;
    if (undo) {
      toast(msg || "Geändert", "ok", {
        label: "Rückgängig",
        fn: () => plEdit([{ op: "replace", tracks: before.tracks }], { okMsg: "Rückgängig gemacht", quiet: true }),
      });
    } else if (msg && !quiet) {
      toast(msg, "ok");
    } else if (msg) {
      toast(msg, "ok");
    }
    return r;
  } catch (e) {
    if (e instanceof Unauthorized) return;
    toast(e.message, "error");
    showPlaylist(before.name);
  }
}

function tracksAt(keys) {
  return keys.map((i) => pl.data.tracks[i]).filter(Boolean);
}

async function enqueueTracks(tracks, opts = {}) {
  const voice_channel = $("#pl-play select")?.value || $("#voice-select").value || undefined;
  const r = await run(null, () => api("POST", "/player/enqueue", { tracks, voice_channel, ...opts }), (r) =>
    opts.now ? "Spielt jetzt" : r.added === 1 ? (r.position === 0 ? "Spielt jetzt" : `Eingereiht an Position ${r.position}`) : `${r.added} Titel eingereiht`);
  refreshPlayer();
  return r;
}

async function copyToPlaylist(target, keys, move) {
  const name = pl.data.name;
  const r = await run(null, () => api("POST", `${plPath(target)}/append`, { tracks: tracksAt(keys) }));
  if (!r) return;
  if (move) {
    await plEdit([{ op: "remove", indices: keys }], { okMsg: `${keys.length} Titel nach „${target}“ verschoben`, undo: false });
  } else {
    toast(`${keys.length} Titel nach „${target}“ kopiert`, "ok");
    loadPlaylists();
  }
  if (!move && pl.data && sameName(pl.data.name, name)) plList.list.clearSelection();
}

function choosePlaylist(button, keys, move) {
  const others = state.playlists.filter((n) => !sameName(n, pl.data.name));
  TrackList.menuAt(button, [
    { heading: move ? "Verschieben nach" : "Kopieren nach" },
    ...others.map((n) => ({ label: n, sub: true, fn: () => copyToPlaylist(n, keys, move) })),
    { label: "＋ Neue Playlist …", sub: true, fn: () => {
      const n = prompt("Name der neuen Playlist:");
      if (n) copyToPlaylist(n.trim(), keys, move);
    } },
  ]);
}

function plTrackAction(act, keys, btn) {
  const tracks = tracksAt(keys);
  const n = pl.data.tracks.length;
  switch (act) {
    case "now": return enqueueTracks(tracks, { now: true });
    case "next": return enqueueTracks(tracks, { next: true });
    case "queue": return enqueueTracks(tracks);
    case "from": return run(null, () => api("POST", `${plPath(pl.data.name)}/play`, { start: keys[0], voice_channel: $("#pl-play select").value || undefined }),
      (r) => `${r.added} Titel ab Position ${keys[0] + 1} eingereiht`).then(refreshPlayer);
    case "top": return plEdit([{ op: "move", indices: keys, to: 0 }], { quiet: true, optimistic: () => movedLocally(pl.data.tracks, keys, 0) });
    case "bottom": return plEdit([{ op: "move", indices: keys, to: n }], { quiet: true, optimistic: () => movedLocally(pl.data.tracks, keys, n) });
    case "copy": return btn ? choosePlaylist(btn, keys, false) : null;
    case "move": return btn ? choosePlaylist(btn, keys, true) : null;
    case "link": return copyLink(tracks[0].url);
    case "remove":
      return plEdit([{ op: "remove", indices: keys }], {
        undo: true,
        okMsg: keys.length === 1 ? `„${tracks[0].title}“ entfernt` : `${keys.length} Titel entfernt`,
      });
  }
}

function plTrackMenu(i) {
  const n = pl.data.tracks.length;
  const others = state.playlists.filter((x) => !sameName(x, pl.data.name));
  return [
    { label: "▶ Jetzt spielen", fn: () => plTrackAction("now", [i]) },
    { label: "⏭ Als Nächstes", fn: () => plTrackAction("next", [i]) },
    { label: "➕ Ans Ende der Warteschlange", fn: () => plTrackAction("queue", [i]) },
    { label: "⤵ Playlist ab hier abspielen", fn: () => plTrackAction("from", [i]) },
    "-",
    { label: "⤒ Nach oben", disabled: i === 0, fn: () => plTrackAction("top", [i]) },
    { label: "⤓ Nach unten", disabled: i === n - 1, fn: () => plTrackAction("bottom", [i]) },
    ...(others.length ? ["-", { heading: "Kopieren nach" }, ...others.map((x) => ({ label: x, sub: true, fn: () => copyToPlaylist(x, [i], false) }))] : []),
    "-",
    { label: "🔗 Link kopieren", fn: () => plTrackAction("link", [i]) },
    { label: "✕ Entfernen", danger: true, fn: () => plTrackAction("remove", [i]) },
  ];
}

function playlistMenu() {
  const p = pl.data;
  const sort = (by, label) => ({
    label, sub: true,
    fn: () => plEdit([{ op: "sort", by }], { undo: true, okMsg: "Sortiert" }),
  });
  const running = pl.check?.running;
  return [
    { label: "✎ Umbenennen …", fn: renamePlaylist },
    { label: "⧉ Duplizieren …", fn: duplicatePlaylist },
    "-",
    { heading: "Sortieren" },
    sort("title", "nach Titel"),
    sort("author", "nach Interpret"),
    sort("duration", "nach Dauer"),
    sort("reverse", "Reihenfolge umkehren"),
    sort("shuffle", "dauerhaft mischen"),
    "-",
    { label: "♊ Duplikate entfernen", fn: () => plEdit([{ op: "dedupe" }], {
      undo: true,
      okMsg: (r) => r.affected ? `${r.affected} doppelte Titel entfernt` : "Keine Duplikate gefunden",
    }) },
    running
      ? { label: "■ Prüfung abbrechen", fn: cancelCheck }
      : { label: "🩺 Verfügbarkeit prüfen", disabled: !p.tracks.length, fn: startCheck },
    "-",
    p.source
      ? { label: "↻ Aus Quelle aktualisieren", fn: () => syncPlaylist(null) }
      : null,
    { label: p.source ? "Quelle ändern …" : "🔗 Quelle festlegen …", fn: setSource },
    p.source ? { label: "Quelle entfernen", fn: () => putSource(null) } : null,
    "-",
    { label: "⤓ Exportieren (JSON)", fn: exportPlaylist },
    { label: "🗑 Playlist löschen", danger: true, fn: deletePlaylist },
  ].filter(Boolean);
}

async function renamePlaylist() {
  const old = pl.data.name;
  const name = prompt("Neuer Name:", old);
  if (!name || name.trim() === old) return;
  const r = await run(null, () => api("POST", `${plPath(old)}/rename`, { name }), "Umbenannt");
  if (!r) return;
  if (state.autoplaylist && sameName(state.autoplaylist, old)) state.autoplaylist = r.name;
  pl.data = null;
  await loadPlaylists();
  showPlaylist(r.name);
}

async function duplicatePlaylist() {
  const name = prompt("Name der Kopie:", `${pl.data.name} (Kopie)`);
  if (!name) return;
  const r = await run(null, () => api("POST", `${plPath(pl.data.name)}/duplicate`, { name }), "Kopie angelegt");
  if (!r) return;
  await loadPlaylists();
  showPlaylist(r.name);
}

async function deletePlaylist() {
  const name = pl.data.name;
  const auto = state.autoplaylist && sameName(state.autoplaylist, name) ? "\nSie ist als Autoplaylist eingestellt!" : "";
  if (!confirm(`Playlist „${name}“ mit ${pl.data.tracks.length} Titeln endgültig löschen?${auto}`)) return;
  const r = await run(null, () => api("DELETE", plPath(name)), "Gelöscht");
  if (!r) return;
  closePlaylist();
  loadPlaylists();
}

async function exportPlaylist() {
  const data = await run(null, () => api("GET", `${plPath(pl.data.name)}/export`));
  if (!data) return;
  const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = `${data.name.replace(/[\\/:*?"<>|]+/g, "_")}.lemusic.json`;
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(a.href), 5000);
}

async function putSource(source) {
  const name = pl.data.name;
  const r = await run(null, () => api("PUT", `${plPath(name)}/source`, { source }), source ? "Quelle gespeichert" : "Quelle entfernt");
  if (r) showPlaylist(name);
}

function setSource() {
  const s = prompt("Link der YouTube-/SoundCloud-Playlist, aus der „Aktualisieren“ neue Titel holt:", pl.data.source || "");
  if (s != null) putSource(s.trim() || null);
}

async function syncPlaylist(button) {
  const r = await run(button, () => api("POST", `${plPath(pl.data.name)}/sync`), (r) =>
    r.added ? `${r.added} neue Titel übernommen` : "Keine neuen Titel in der Quelle");
  if (r) {
    setPlaylist(r);
    loadPlaylists();
  }
}

// Verfügbarkeitsprüfung -----------------------------------------------------

function stopCheckPolling() {
  clearTimeout(pl.checkTimer);
  pl.checkTimer = 0;
}

async function pollCheck(name, initial = false) {
  stopCheckPolling();
  let status;
  try {
    status = await api("GET", `${plPath(name)}/check`);
  } catch {
    return;
  }
  if (!pl.data || !sameName(pl.data.name, name)) return;
  const wasRunning = pl.check?.running;
  pl.check = status;
  renderCheck();
  renderPlaylistItems();
  if (status?.running) {
    pl.checkTimer = setTimeout(() => pollCheck(name), 2000);
  } else if (wasRunning && !initial && status) {
    toast(status.gone.length ? `Prüfung fertig: ${status.gone.length} Titel nicht mehr verfügbar` : "Prüfung fertig: alle Titel verfügbar",
      status.gone.length ? "error" : "ok");
  }
}

async function startCheck() {
  const name = pl.data.name;
  const r = await run(null, () => api("POST", `${plPath(name)}/check`), "Prüfung gestartet – läuft im Hintergrund weiter");
  if (r) pollCheck(name);
}

async function cancelCheck() {
  const name = pl.data.name;
  await run(null, () => api("DELETE", `${plPath(name)}/check`), "Prüfung wird abgebrochen");
  pollCheck(name);
}

function renderCheck() {
  const box = $("#pl-check");
  if (!box) return;
  const c = pl.check;
  if (!c) {
    box.hidden = true;
    return;
  }
  box.hidden = false;
  const goneHere = pl.data.tracks.filter((t) => c.gone.some((g) => g.url === t.url)).length;
  const pct = c.total ? Math.round((c.done / c.total) * 100) : 0;
  const unknown = c.unknown ? ` · ${c.unknown} nicht prüfbar (später erneut versuchen)` : "";
  if (c.running) {
    box.className = "check-box";
    box.innerHTML = `
      <div class="row"><strong>🩺 Prüfe Verfügbarkeit … ${c.done} / ${c.total}</strong>
        <span class="muted">${c.gone.length} nicht verfügbar${esc(unknown)}</span>
        <button class="btn small" id="pl-check-cancel">Abbrechen</button></div>
      <div class="progress"><div class="progress-fill" id="pl-check-bar"></div></div>`;
    $("#pl-check-bar").style.width = `${pct}%`;
    $("#pl-check-cancel").addEventListener("click", cancelCheck);
  } else {
    box.className = `check-box ${goneHere ? "bad" : "good"}`;
    box.innerHTML = goneHere
      ? `<div class="row"><strong>⚠ ${goneHere} Titel nicht mehr verfügbar</strong><span class="muted">(rot markiert${esc(unknown)})</span>
          <button class="btn small danger" id="pl-check-remove">Alle entfernen</button>
          <button class="btn small ghost" id="pl-check-close">Ausblenden</button></div>`
      : `<div class="row"><strong>✅ ${c.cancelled ? `Abgebrochen nach ${c.done} von ${c.total} Titeln – bis dahin alles verfügbar` : "Alle Titel verfügbar"}</strong>
          <span class="muted">${esc(unknown)}</span><button class="btn small ghost" id="pl-check-close">Ausblenden</button></div>`;
    $("#pl-check-close").addEventListener("click", () => {
      pl.check = null;
      renderCheck();
      renderPlaylistItems();
    });
    $("#pl-check-remove")?.addEventListener("click", () => {
      const gone = new Set(c.gone.map((g) => g.url));
      const indices = pl.data.tracks.map((t, i) => (gone.has(t.url) ? i : -1)).filter((i) => i >= 0);
      plEdit([{ op: "remove", indices }], { undo: true, okMsg: `${indices.length} nicht verfügbare Titel entfernt` });
    });
  }
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
    f.public_enabled.checked = bot.public_enabled;
    f.public_pin.value = bot.public_pin;
    f.public_url.value = bot.public_url;
    f.ytdlp_auto_update.checked = bot.ytdlp_auto_update;
    f.ytdlp_update_time.value = bot.ytdlp_update_time;
    updatePublicNote();
    updateYtdlpNote();
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
    f.auto_join.checked = guild.auto_join;
    const picks = $("#auto-join-channels .picks");
    const chosen = new Set(guild.auto_join_channels ?? []);
    picks.innerHTML = (g?.voice_channels ?? []).map((c) => `
      <label class="check"><input type="checkbox" data-channel="${esc(c.id)}" ${chosen.has(c.id) ? "checked" : ""}> ${esc(c.name)}</label>`).join("")
      || `<span class="muted">Sprachkanäle erscheinen, sobald der Bot mit Discord verbunden ist.</span>`;
    updateAutoJoin();
  }
}

function renderYtdlpLast(s) {
  const el = $("#ytdlp-last");
  el.textContent = s
    ? `Letzte Prüfung (${s.trigger}): ${s.at} – ${s.ok ? "✅" : "❌"} ${s.message}`
    : "Noch keine Prüfung seit dem Update auf diese Version.";
}

function updateYtdlpNote() {
  const f = $("#bot-settings");
  f.ytdlp_update_time.disabled = !f.ytdlp_auto_update.checked;
  $("#ytdlp-schedule-note").textContent = f.ytdlp_auto_update.checked
    ? `Zeitzone: ${state.timeZone || "Europe/Berlin"} (änderbar über die Umgebungsvariable TZ). Zusätzlich wird bei jedem Containerstart aktualisiert.`
    : "Aus – dann nur beim Containerstart oder über System → „Jetzt aktualisieren“.";
}
$("#bot-settings [name=ytdlp_auto_update]").addEventListener("change", updateYtdlpNote);

function updateAutoJoin() {
  $("#auto-join-channels").disabled = !$("#guild-settings").auto_join.checked;
}
$("#guild-settings [name=auto_join]").addEventListener("change", updateAutoJoin);

function updatePublicNote() {
  const f = $("#bot-settings");
  const base = (f.public_url.value.trim() || location.origin).replace(/\/+$/, "");
  const note = $("#public-link-note");
  note.replaceChildren();
  if (!f.public_enabled.checked) {
    note.textContent = "Deaktiviert.";
    return;
  }
  const a = document.createElement("a");
  a.href = `${base}/public`;
  a.target = "_blank";
  a.rel = "noopener";
  a.textContent = `${base}/public`;
  note.append("Link: ", a);
  if (!f.public_url.value.trim()) note.append(" – ohne öffentliche Adresse postet der Bot keine Links in Discord.");
}
["public_enabled", "public_url"].forEach((n) =>
  $(`#bot-settings [name=${n}]`).addEventListener("input", updatePublicNote));

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
    auto_join: d.auto_join,
    auto_join_channels: $$("#auto-join-channels input[data-channel]:checked").map((i) => i.dataset.channel),
  };
  run($("button", ev.target), () => api("PUT", "/settings/guild", body), "Gespeichert").then((r) => {
    if (r) state.autoplaylist = body.autoplaylist;
  });
});

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

$("#btn-ytdlp-update").addEventListener("click", (ev) => {
  const btn = ev.currentTarget;
  btn.textContent = "Aktualisiere …";
  run(btn, () => api("POST", "/system/ytdlp-update"), (r) => `yt-dlp ${r.version} – ${r.status.message}`).finally(() => {
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
