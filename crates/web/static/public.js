"use strict";

// Öffentliches Dashboard: Wiedergabe ansehen, Links hinzufügen.

const $ = (sel, root = document) => root.querySelector(sel);

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

class Locked extends Error {}

async function api(method, path, body) {
  const opts = { method, headers: { "X-LeMusic": "1" }, credentials: "same-origin" };
  if (body !== undefined) {
    opts.body = JSON.stringify(body);
    opts.headers["Content-Type"] = "application/json";
  }
  const res = await fetch(`/public/api${path}`, opts);
  let data = null;
  try { data = await res.json(); } catch { /* leer */ }
  if (res.status === 404) {
    show("disabled");
    throw new Locked();
  }
  if (res.status === 401 && path !== "/unlock") {
    show("pin");
    throw new Locked();
  }
  if (!res.ok) throw new Error(data?.error || `Fehler ${res.status}`);
  return data;
}

function show(view) {
  for (const v of ["disabled", "pin", "main"]) $(`#${v}-view`).hidden = v !== view;
  if (view === "pin") $("#pin-form input").focus();
  if (view !== "main") stopTimers();
}

const state = { player: null, fetchedAt: 0, timers: [] };

function stopTimers() {
  state.timers.forEach(clearInterval);
  state.timers = [];
}

// ---------------------------------------------------------------------------
// Wiedergabe
// ---------------------------------------------------------------------------

async function refresh() {
  try {
    const r = await api("GET", "/player");
    state.player = r.player;
    state.fetchedAt = Date.now();
    render(r);
  } catch (e) {
    if (!(e instanceof Locked)) console.warn(e);
  }
}

function render({ player: p, voice_channels }) {
  // Sprachkanal nur abfragen, wenn der Bot nicht verbunden ist.
  const voiceRow = $("#voice-row");
  voiceRow.hidden = Boolean(p.voice_channel);
  const sel = $("#voice-select");
  const prev = sel.value;
  sel.replaceChildren(...voice_channels.map((c) => new Option(c.name, c.id)));
  if (prev) sel.value = prev;

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
    $("#now-state").textContent = now.paused ? "Pausiert" : now.ready ? "Läuft" : "Lädt …";
    title.textContent = now.entry.track.title;
    title.href = now.entry.track.url;
    $("#now-author").textContent = now.entry.track.author;
  } else {
    $("#now-state").textContent = "Nichts läuft";
    title.textContent = "Wünsch dir was!";
    title.removeAttribute("href");
    $("#now-author").textContent = "Zieh einen Link in das Feld oben.";
  }
  tick();

  $("#queue-meta").textContent = p.queue.length ? `· ${p.queue.length} Titel · ${fmt(p.queue_duration)}` : "";
  const list = $("#queue");
  list.innerHTML = p.queue.length
    ? p.queue.slice(0, 30).map((e, i) => `
        <li>
          <span class="pos">${i + 1}</span>
          ${e.track.thumbnail ? `<img loading="lazy" alt="" src="${esc(e.track.thumbnail)}">` : ""}
          <div class="meta">
            <a href="${esc(e.track.url)}" target="_blank" rel="noopener">${esc(e.track.title)}</a>
            <small>${esc(e.track.author)}${e.autoplay ? " · 🔁 Autoplaylist" : " · 🙋 Wunsch"}</small>
          </div>
          <span class="dur">${fmt(e.track.duration)}</span>
        </li>`).join("") + (p.queue.length > 30 ? `<li class="empty">… und ${p.queue.length - 30} weitere</li>` : "")
    : `<li class="empty">Die Warteschlange ist leer.</li>`;
}

function tick() {
  const now = state.player?.now;
  if (!now) {
    $("#now-progress").style.width = "0%";
    $("#now-pos").textContent = "0:00";
    $("#now-len").textContent = "0:00";
    return;
  }
  let pos = now.position;
  if (!now.paused && now.ready) pos += (Date.now() - state.fetchedAt) / 1000;
  const len = now.entry.track.duration;
  if (len) pos = Math.min(pos, len);
  $("#now-pos").textContent = fmt(pos);
  $("#now-len").textContent = len ? fmt(len) : "Live";
  $("#now-progress").style.width = len ? `${(pos / len) * 100}%` : "100%";
}

// ---------------------------------------------------------------------------
// Hinzufügen
// ---------------------------------------------------------------------------

let busy = false;

async function addLinks(links) {
  if (busy) return toast("Moment, der letzte Link wird noch geladen …");
  busy = true;
  const zone = $("#drop-zone");
  zone.classList.add("drop-busy");
  try {
    const body = { links };
    if (!$("#voice-row").hidden) body.voice_channel = $("#voice-select").value;
    const r = await api("POST", "/add", body);
    let msg = r.added === 1
      ? (r.position === 0 ? "Spielt jetzt!" : `Eingereiht an Position ${r.position}`)
      : `${r.added} Titel eingereiht`;
    if (r.truncated) msg += " (Playlist gekürzt)";
    toast(msg, "ok");
    for (const f of r.failed || []) toast(`Nicht geladen: ${f.error}`, "error");
    if (r.skipped_too_long) toast(`${r.skipped_too_long} Titel zu lang – ausgelassen`);
    refresh();
  } catch (e) {
    if (!(e instanceof Locked)) toast(e.message, "error");
  } finally {
    busy = false;
    zone.classList.remove("drop-busy");
  }
}

LeMusicDnD.attach($("#drop-zone"), addLinks, () =>
  toast("Das war kein YouTube- oder SoundCloud-Link", "error"));

$("#add-form").addEventListener("submit", (ev) => {
  ev.preventDefault();
  const input = ev.target.elements.link;
  const links = LeMusicDnD.linksFromText(input.value);
  if (!links.length) return toast("Bitte einen YouTube- oder SoundCloud-Link einfügen", "error");
  addLinks(links).then(() => (input.value = ""));
});

// Einfügen (Strg+V) irgendwo auf der Seite.
document.addEventListener("paste", (ev) => {
  if (ev.target.closest?.("input, textarea")) return;
  const links = LeMusicDnD.linksFromText(ev.clipboardData?.getData("text"));
  if (links.length) addLinks(links);
});

// ---------------------------------------------------------------------------
// PIN & Start
// ---------------------------------------------------------------------------

$("#pin-form").addEventListener("submit", async (ev) => {
  ev.preventDefault();
  const btn = $("button", ev.target);
  btn.disabled = true;
  $("#pin-error").textContent = "";
  try {
    await api("POST", "/unlock", { pin: ev.target.elements.pin.value });
    ev.target.reset();
    start();
  } catch (e) {
    if (!(e instanceof Locked)) $("#pin-error").textContent = e.message;
  } finally {
    btn.disabled = false;
  }
});

async function start() {
  let status;
  try {
    status = await api("GET", "/status");
  } catch (e) {
    if (!(e instanceof Locked)) toast(e.message, "error");
    return;
  }
  if (!status.unlocked) return show("pin");
  $("#guild-name").textContent = status.guild_name ? `· ${status.guild_name}` : "";
  show("main");
  await refresh();
  stopTimers();
  state.timers.push(setInterval(refresh, 3000), setInterval(tick, 1000));
}

start();
